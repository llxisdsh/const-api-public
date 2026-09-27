//! WebRTC edges for platform voice. Only encoded Opus and data-channel frames
//! cross the existing platform tunnel; SDP/ICE credentials stay at each edge.
//! There is no codec, microphone access, media persistence or replay here.
use anyhow::{Context, Result, anyhow, bail};
use bytes::BytesMut;
use rtc::{
    interceptor::Registry,
    media_stream::MediaStreamTrack,
    peer_connection::{
        configuration::{
            interceptor_registry::register_default_interceptors,
            media_engine::{MIME_TYPE_OPUS, MediaEngine},
            setting_engine::SettingEngine,
        },
        sdp::RTCSessionDescription,
        transport::RTCIceCandidateInit,
    },
    rtp::Packet,
    rtp_transceiver::rtp_sender::{
        RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters,
        RtpCodecKind,
    },
    shared::marshal::{Marshal, Unmarshal},
};
use std::{sync::Arc, time::Duration};
use tokio::sync::{Mutex, OnceCell, mpsc, watch};
use webrtc::{
    data_channel::{DataChannel, DataChannelEvent},
    media_stream::{
        track_local::{TrackLocal, static_rtp::TrackLocalStaticRTP},
        track_remote::{TrackRemote, TrackRemoteEvent},
    },
    peer_connection::{
        PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCIceGatheringState,
        RTCPeerConnectionState,
    },
    rtp_transceiver::RtpSender,
};

// Versioned binary lanes. Text sideband events remain ordinary tunnel text.
pub(crate) const FRAME_PREFIX: &[u8] = b"CRTC\x01";
pub(crate) const AUDIO: u8 = 1;
pub(crate) const DATA_TEXT: u8 = 2;
pub(crate) const DATA_BINARY: u8 = 3;
const MAX_DATA_BYTES: usize = 1024 * 1024;
const IO_WAIT: Duration = Duration::from_secs(15);

pub(crate) fn frame(lane: u8, payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(FRAME_PREFIX.len() + 1 + payload.len());
    bytes.extend_from_slice(FRAME_PREFIX);
    bytes.push(lane);
    bytes.extend_from_slice(payload);
    bytes
}

pub(crate) fn parse_frame(bytes: &[u8]) -> Result<(u8, &[u8])> {
    let bytes = bytes
        .strip_prefix(FRAME_PREFIX)
        .context("invalid voice media frame")?;
    let (&lane, payload) = bytes.split_first().context("empty voice media frame")?;
    if !matches!(lane, AUDIO | DATA_TEXT | DATA_BINARY) || payload.len() > MAX_DATA_BYTES {
        bail!("unsupported or oversized voice media frame");
    }
    Ok((lane, payload))
}

struct Handler {
    output: mpsc::Sender<Vec<u8>>,
    stopped: watch::Sender<bool>,
    gathered: watch::Sender<bool>,
    connected: watch::Sender<bool>,
    data_ready: watch::Sender<bool>,
    data: Mutex<Option<Arc<dyn DataChannel>>>,
}

impl Handler {
    fn stop(&self) {
        self.stopped.send_replace(true);
    }

    fn emit(&self, bytes: Vec<u8>) -> bool {
        // A stalled tunnel must not retain seconds of stale speech or unbounded
        // provider events. End this call instead of replaying old audio later.
        if self.output.try_send(bytes).is_err() {
            self.stop();
            return false;
        }
        true
    }

    async fn attach_data(self: &Arc<Self>, data: Arc<dyn DataChannel>) {
        let mut slot = self.data.lock().await;
        if slot.is_some() {
            self.stop();
            return;
        }
        *slot = Some(data.clone());
        drop(slot);
        let handler = self.clone();
        tokio::spawn(async move {
            let mut stopped = handler.stopped.subscribe();
            loop {
                tokio::select! {
                    _ = wait_stopped(&mut stopped) => break,
                    event = data.poll() => match event {
                        Some(DataChannelEvent::OnOpen) => { handler.data_ready.send_replace(true); }
                        Some(DataChannelEvent::OnMessage(message)) => {
                            if message.data.len() > MAX_DATA_BYTES || !handler.emit(frame(
                                if message.is_string { DATA_TEXT } else { DATA_BINARY }, &message.data,
                            )) { break; }
                        }
                        None | Some(DataChannelEvent::OnClose | DataChannelEvent::OnError) => break,
                        _ => {}
                    }
                }
            }
            handler.stop();
        });
    }
}

struct Events(Arc<Handler>);
impl std::ops::Deref for Events {
    type Target = Arc<Handler>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
#[async_trait::async_trait]
impl PeerConnectionEventHandler for Events {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            self.gathered.send_replace(true);
        }
    }
    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        if state == RTCPeerConnectionState::Connected {
            self.connected.send_replace(true);
        }
        if matches!(
            state,
            RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed
        ) {
            self.stop();
        }
    }
    async fn on_data_channel(&self, data: Arc<dyn DataChannel>) {
        self.attach_data(data).await;
    }
    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        if track.kind().await != RtpCodecKind::Audio {
            self.stop();
            return;
        }
        let handler = self.0.clone();
        tokio::spawn(async move {
            let mut stopped = handler.stopped.subscribe();
            loop {
                tokio::select! {
                    _ = wait_stopped(&mut stopped) => break,
                    event = track.poll() => match event {
                        Some(TrackRemoteEvent::OnRtpPacket(packet)) => {
                            let Ok(bytes) = packet.marshal() else { handler.stop(); break; };
                            if !handler.emit(frame(AUDIO, &bytes)) { break; }
                        }
                        None | Some(TrackRemoteEvent::OnError | TrackRemoteEvent::OnEnded) => break,
                        _ => {}
                    }
                }
            }
        });
    }
}

pub(crate) struct MediaPeer {
    pc: Arc<dyn PeerConnection>,
    handler: Arc<Handler>,
    track: Arc<TrackLocalStaticRTP>,
    sender: Arc<dyn RtpSender>,
    encoding: OnceCell<(u32, u8)>,
    pub(crate) received: mpsc::Receiver<Vec<u8>>,
}

impl Drop for MediaPeer {
    fn drop(&mut self) {
        self.handler.stop();
        let pc = self.pc.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = tokio::time::timeout(IO_WAIT, pc.close()).await;
            });
        }
    }
}

impl MediaPeer {
    pub(crate) async fn new(loopback: bool) -> Result<Self> {
        let codec = RTCRtpCodec {
            mime_type: MIME_TYPE_OPUS.into(),
            clock_rate: 48_000,
            channels: 2,
            sdp_fmtp_line: "minptime=10;useinbandfec=1".into(),
            ..Default::default()
        };
        let mut engine = MediaEngine::default();
        engine.register_codec(
            RTCRtpCodecParameters {
                rtp_codec: codec.clone(),
                payload_type: 111,
            },
            RtpCodecKind::Audio,
        )?;
        let registry = register_default_interceptors(Registry::new(), &mut engine)?;
        let mut settings = SettingEngine::default();
        settings.set_include_loopback_candidate(true);
        // Match Codex's negotiation budget; the library's short default can
        // exhaust ICE checks while TCP is still connecting.
        settings.set_ice_connection_attempts(Some(Duration::from_millis(200)), Some(75));
        let (output, received) = mpsc::channel(128);
        let handler = Arc::new(Handler {
            output,
            stopped: watch::channel(false).0,
            gathered: watch::channel(false).0,
            connected: watch::channel(false).0,
            data_ready: watch::channel(false).0,
            data: Mutex::new(None),
        });
        let pc: Arc<dyn PeerConnection> = Arc::new(
            PeerConnectionBuilder::new()
                .with_media_engine(engine)
                .with_interceptor_registry(registry)
                .with_setting_engine(settings)
                .with_handler(Arc::new(Events(handler.clone())))
                .with_data_channel_send_buffer_limit(MAX_DATA_BYTES)
                .with_sctp_receive_buffer_size(MAX_DATA_BYTES as u32)
                .with_udp_addrs(if loopback {
                    vec!["127.0.0.1:0"]
                } else {
                    vec!["0.0.0.0:0", "[::]:0", "127.0.0.1:0"]
                })
                .with_tcp_addrs(if loopback {
                    vec!["127.0.0.1:0"]
                } else {
                    vec!["0.0.0.0:0", "[::]:0", "127.0.0.1:0"]
                })
                .build()
                .await?,
        );
        let track = Arc::new(TrackLocalStaticRTP::new(MediaStreamTrack::new(
            "const-voice".into(),
            "audio".into(),
            "audio".into(),
            RtpCodecKind::Audio,
            vec![RTCRtpEncodingParameters {
                rtp_coding_parameters: RTCRtpCodingParameters {
                    ssrc: Some(rand::random()),
                    ..Default::default()
                },
                codec,
                ..Default::default()
            }],
        )));
        let sender = match pc.add_track(track.clone() as Arc<dyn TrackLocal>).await {
            Ok(sender) => sender,
            Err(error) => {
                let _ = pc.close().await;
                return Err(error.into());
            }
        };
        let (feedback_track, feedback_handler) = (track.clone(), handler.clone());
        tokio::spawn(async move {
            let mut stopped = feedback_handler.stopped.subscribe();
            let mut connected = feedback_handler.connected.subscribe();
            tokio::select! {
                _ = wait_stopped(&mut stopped) => return,
                _ = wait_stopped(&mut connected) => {},
            }
            loop {
                tokio::select! {
                    _ = wait_stopped(&mut stopped) => break,
                    event = feedback_track.poll() => if event.is_none() { break; },
                }
            }
        });
        Ok(Self {
            pc,
            handler,
            track,
            sender,
            encoding: OnceCell::new(),
            received,
        })
    }

    async fn local_sdp(&self) -> Result<String> {
        wait_flag(&self.handler.gathered)
            .await
            .context("voice ICE gathering timed out")?;
        self.pc
            .local_description()
            .await
            .map(|desc| desc.sdp)
            .context("voice SDP missing")
    }

    pub(crate) async fn offer(&self) -> Result<String> {
        let data = self.pc.create_data_channel("oai-events", None).await?;
        self.handler.attach_data(data).await;
        self.pc
            .set_local_description(self.pc.create_offer(None).await?)
            .await?;
        self.local_sdp().await
    }

    pub(crate) async fn answer(&self, offer: &str) -> Result<String> {
        validate_audio_sdp(offer)?;
        self.install_remote(RTCSessionDescription::offer(offer.to_string())?)
            .await?;
        self.pc
            .set_local_description(self.pc.create_answer(None).await?)
            .await?;
        self.local_sdp().await
    }

    pub(crate) async fn accept_answer(&self, answer: &str) -> Result<()> {
        validate_audio_sdp(answer)?;
        self.install_remote(RTCSessionDescription::answer(answer.to_string())?)
            .await?;
        Ok(())
    }

    async fn install_remote(&self, description: RTCSessionDescription) -> Result<()> {
        let parsed = description.unmarshal()?;
        let mut candidates = Vec::new();
        for attribute in parsed
            .media_descriptions
            .iter()
            .flat_map(|media| &media.attributes)
            .filter(|attribute| attribute.key == "candidate")
        {
            if candidates.len() >= 64 {
                bail!("too many voice ICE candidates");
            }
            candidates.push(RTCIceCandidateInit {
                candidate: format!(
                    "candidate:{}",
                    attribute
                        .value
                        .as_deref()
                        .context("invalid voice ICE candidate")?
                ),
                ..Default::default()
            });
        }
        self.pc.set_remote_description(description).await?;
        // The async driver needs explicit candidates to initiate ICE-TCP even
        // when they were already present in the non-trickle SDP answer.
        for candidate in candidates {
            self.pc.add_ice_candidate(candidate).await?;
        }
        Ok(())
    }

    pub(crate) fn stopped(&self) -> watch::Receiver<bool> {
        self.handler.stopped.subscribe()
    }

    pub(crate) fn is_connected(&self) -> bool {
        *self.handler.connected.borrow() && *self.handler.data_ready.borrow()
    }

    pub(crate) async fn wait_connected(&self) -> Result<()> {
        wait_flag(&self.handler.connected).await?;
        wait_flag(&self.handler.data_ready).await
    }

    pub(crate) async fn send(&self, bytes: &[u8]) -> Result<()> {
        let (lane, payload) = parse_frame(bytes)?;
        tokio::time::timeout(IO_WAIT, async {
            if lane == AUDIO {
                wait_flag(&self.handler.connected).await?;
                let (ssrc, pt) = *self
                    .encoding
                    .get_or_try_init(|| async {
                        let params = self.sender.get_parameters().await?;
                        let ssrc = params
                            .encodings
                            .first()
                            .and_then(|encoding| encoding.rtp_coding_parameters.ssrc)
                            .context("voice sender has no negotiated SSRC")?;
                        let pt = params
                            .rtp_parameters
                            .codecs
                            .iter()
                            .find(|codec| {
                                codec
                                    .rtp_codec
                                    .mime_type
                                    .eq_ignore_ascii_case(MIME_TYPE_OPUS)
                            })
                            .map(|codec| codec.payload_type)
                            .context("voice peer did not negotiate Opus")?;
                        Ok::<_, anyhow::Error>((ssrc, pt))
                    })
                    .await?;
                let mut packet = Packet::unmarshal(&mut BytesMut::from(payload))?;
                packet.header.ssrc = ssrc;
                packet.header.payload_type = pt;
                // RTP extension IDs are local to each negotiation. Do not leak
                // source MID/transport sequence IDs onto the other peer.
                packet.header.extension = false;
                packet.header.extensions.clear();
                packet.header.extension_profile = 0;
                self.track.write_rtp(packet).await?;
            } else {
                wait_flag(&self.handler.data_ready).await?;
                let data = self
                    .handler
                    .data
                    .lock()
                    .await
                    .clone()
                    .context("voice data channel missing")?;
                if lane == DATA_TEXT {
                    data.send_text(std::str::from_utf8(payload)?).await?;
                } else {
                    data.send(BytesMut::from(payload)).await?;
                }
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .map_err(|_| anyhow!("voice media delivery timed out"))?
    }
}

async fn wait_flag(flag: &watch::Sender<bool>) -> Result<()> {
    let mut receiver = flag.subscribe();
    tokio::time::timeout(IO_WAIT, receiver.wait_for(|ready| *ready))
        .await
        .context("voice peer not ready")?
        .context("voice peer closed")?;
    Ok(())
}

pub(crate) async fn wait_stopped(receiver: &mut watch::Receiver<bool>) {
    let _ = receiver.wait_for(|stopped| *stopped).await;
}

fn validate_audio_sdp(sdp: &str) -> Result<()> {
    let media: Vec<_> = sdp
        .lines()
        .filter_map(|line| line.strip_prefix("m="))
        .collect();
    if sdp.len() > MAX_DATA_BYTES
        || media
            .iter()
            .filter(|line| line.starts_with("audio "))
            .count()
            != 1
        || media
            .iter()
            .any(|line| !line.starts_with("audio ") && !line.starts_with("application "))
        || media
            .iter()
            .filter(|line| line.starts_with("application "))
            .count()
            > 1
    {
        bail!("platform voice supports one Opus audio track and one data channel");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn voice_media_opus_and_data_round_trip_without_transcoding() -> Result<()> {
        let mut left = MediaPeer::new(true).await?;
        let mut right = MediaPeer::new(true).await?;
        let offer = left.offer().await?;
        let answer = right.answer(&offer).await?;
        left.accept_answer(&answer).await?;
        for (sender, receiver) in [(&left, &mut right.received)] {
            let text = frame(DATA_TEXT, br#"{ "type":"client.event", "future":true }"#);
            sender.send(&text).await?;
            assert_eq!(
                tokio::time::timeout(IO_WAIT, receiver.recv())
                    .await?
                    .unwrap(),
                text
            );
        }
        let binary = frame(DATA_BINARY, &[0, 1, 255, 128]);
        right.send(&binary).await?;
        assert_eq!(
            tokio::time::timeout(IO_WAIT, left.received.recv())
                .await?
                .unwrap(),
            binary
        );
        let packet = Packet {
            header: rtc::rtp::header::Header {
                version: 2,
                sequence_number: 17,
                timestamp: 960,
                ssrc: 45,
                payload_type: 111,
                ..Default::default()
            },
            payload: bytes::Bytes::from_static(&[0xf8, 0xff, 0xfe]),
        };
        left.send(&frame(AUDIO, &packet.marshal()?)).await?;
        let received = tokio::time::timeout(IO_WAIT, right.received.recv())
            .await?
            .unwrap();
        let (lane, payload) = parse_frame(&received)?;
        assert_eq!(lane, AUDIO);
        let received = Packet::unmarshal(&mut BytesMut::from(payload))?;
        assert_eq!(received.payload, packet.payload);
        assert_eq!(
            received.header.sequence_number,
            packet.header.sequence_number
        );
        assert_eq!(received.header.timestamp, packet.header.timestamp);
        Ok(())
    }

    #[test]
    fn voice_media_rejects_unknown_lanes_and_video_without_panicking() {
        assert!(parse_frame(&frame(99, b"test")).is_err());
        assert!(parse_frame(b"bad").is_err());
        assert!(
            validate_audio_sdp("v=0\r\nm=audio 9 RTP/AVP 111\r\nm=video 9 RTP/AVP 96\r\n").is_err()
        );
    }
}
