use super::*;
use crate::surface::{ApiOperation, ApiSurface};
use crate::surface_wire::SurfaceEnvelope;

/// The platform settles admitted videos for up to 24 hours. Withdrawing a
/// channel closes admission but the original transport may still receive a
/// status read for a job it already accepted.
#[derive(Default)]
pub(super) struct AcceptedVideos {
    channels: HashMap<String, ChannelVideos>,
}

struct ChannelVideos {
    latest: Arc<SupplierConfig>,
    jobs: HashMap<String, (Instant, Arc<SupplierConfig>)>,
}

impl AcceptedVideos {
    pub(super) fn remember(
        &mut self,
        supplier: &SupplierConfig,
        response: &serde_json::Map<String, serde_json::Value>,
    ) {
        let Ok(wire) = SurfaceEnvelope::from_payload(response) else {
            return;
        };
        if !wire.status.is_some_and(|s| (200..300).contains(&s)) {
            return;
        }
        #[derive(serde::Deserialize)]
        struct Video {
            id: Option<String>,
            request_id: Option<String>,
        }
        let Some(video) = wire
            .body
            .utf8()
            .ok()
            .and_then(|body| serde_json::from_str::<Video>(body).ok())
        else {
            return;
        };
        let Some(id) = video.id.filter(|s| !s.is_empty()).or(video.request_id) else {
            return;
        };
        if id.is_empty()
            || id.len() > 512
            || matches!(id.as_str(), "." | "..")
            || id.contains(['/', '\\', '?', '#'])
            || id.chars().any(char::is_control)
        {
            return;
        }
        let now = Instant::now();
        self.prune(now);
        let channel = self
            .channels
            .entry(supplier.channel_id.clone())
            .or_insert_with(|| ChannelVideos {
                latest: Arc::new(supplier.clone()),
                jobs: HashMap::new(),
            });
        if *channel.latest != *supplier {
            channel.latest = Arc::new(supplier.clone());
        }
        channel.jobs.insert(
            id,
            (
                now + Duration::from_secs(24 * 60 * 60),
                channel.latest.clone(),
            ),
        );
    }

    fn prune(&mut self, now: Instant) {
        self.channels.retain(|_, channel| {
            channel.jobs.retain(|_, (expiry, _)| *expiry > now);
            !channel.jobs.is_empty()
        });
    }

    pub(super) fn pending(&mut self) -> bool {
        self.prune(Instant::now());
        !self.channels.is_empty()
    }

    pub(super) fn supplier_for_read(
        &self,
        client_id: &str,
        channel: &str,
        unit: &str,
        message: &SupplierMessage,
    ) -> Option<SupplierConfig> {
        if message.kind != "http_request"
            || (channel.is_empty() && unit.is_empty())
            || self.channels.is_empty()
        {
            return None;
        }
        let wire = SurfaceEnvelope::from_payload(&message.payload).ok()?;
        if wire.surface != Some(ApiSurface::OpenAi)
            || wire.method != "GET"
            || !wire.body.data.is_empty()
        {
            return None;
        }
        let segment = wire.path.strip_prefix("/v1/videos/")?;
        let segment = match wire.operation {
            Some(ApiOperation::VideosGet) if !wire.has_query && wire.raw_query.is_empty() => {
                segment
            }
            Some(ApiOperation::VideosContent) => segment.strip_suffix("/content")?,
            _ => return None,
        };
        let id = urlencoding::decode(segment).ok()?;
        self.channels.iter().find_map(|(id_channel, videos)| {
            if (!channel.is_empty() && channel != id_channel)
                || (!unit.is_empty() && supplier_unit_id(client_id, id_channel) != unit)
            {
                return None;
            }
            let (expiry, supplier) = videos.jobs.get(id.as_ref())?;
            (*expiry > Instant::now()).then(|| supplier.as_ref().clone())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface_wire::{WireBody, wire_payload};

    #[test]
    fn withdrawal_allows_only_status_of_a_previously_accepted_task_on_its_owner() {
        let mut videos = AcceptedVideos::default();
        let mut supplier = crate::default_config().supplier;
        supplier.channel_id = "video-owner".into();
        let unit = supplier_unit_id("machine", &supplier.channel_id);
        let response = |status, id: &str| {
            wire_payload(SurfaceEnvelope {
                status: Some(status),
                body: WireBody::from_bytes(
                    &serde_json::to_vec(&serde_json::json!({"request_id":id})).unwrap(),
                ),
                ..Default::default()
            })
            .unwrap()
        };
        videos.remember(&supplier, &response(429, "job-1"));
        assert!(!videos.pending());
        supplier.upstream_base_url = "https://original.invalid".into();
        videos.remember(&supplier, &response(200, "job-1"));
        supplier.upstream_base_url = "https://updated.invalid".into();
        videos.remember(&supplier, &response(200, "new-job"));
        let read = SurfaceEnvelope {
            surface: Some(ApiSurface::OpenAi),
            operation: Some(ApiOperation::VideosGet),
            method: "GET".into(),
            path: "/v1/videos/job-1".into(),
            ..Default::default()
        };
        let owner = |wire, channel, unit, kind: &str| {
            videos.supplier_for_read(
                "machine",
                channel,
                unit,
                &SupplierMessage {
                    id: "poll".into(),
                    kind: kind.into(),
                    payload: wire_payload(wire).unwrap(),
                    output_credit: None,
                },
            )
        };
        for (id, url) in [
            ("job-1", "https://original.invalid"),
            ("new-job", "https://updated.invalid"),
        ] {
            let mut query = read.clone();
            query.path = format!("/v1/videos/{id}");
            assert_eq!(
                owner(query, "video-owner", &unit, "http_request")
                    .unwrap()
                    .upstream_base_url,
                url
            );
        }
        let allowed = |wire, channel, unit, kind: &str| owner(wire, channel, unit, kind).is_some();
        assert!(allowed(read.clone(), "video-owner", &unit, "http_request"));
        assert!(!allowed(read.clone(), "other", &unit, "http_request"));
        assert!(!allowed(
            read.clone(),
            "video-owner",
            "other",
            "http_request"
        ));
        assert!(!allowed(read.clone(), "", "", "http_request"));
        assert!(!allowed(read.clone(), "video-owner", &unit, "duplex_open"));
        for wire in [
            SurfaceEnvelope {
                method: "POST".into(),
                ..read.clone()
            },
            SurfaceEnvelope {
                path: "/v1/videos/job-2".into(),
                ..read.clone()
            },
            SurfaceEnvelope {
                has_query: true,
                raw_query: "other=job".into(),
                ..read.clone()
            },
            SurfaceEnvelope {
                body: WireBody::from_bytes(b"{}"),
                ..read
            },
        ] {
            assert!(!allowed(wire, "video-owner", &unit, "http_request"));
        }
        let channel = supplier.channel_id.clone();
        let mut active = SupplierAgentConfig::single(supplier);
        active.client_id = "machine".into();
        let tasks = SupplierRequestTasks::default();
        *tasks.accepted_videos() = videos;
        let request = |operation, path: &str, query: &str| SupplierMessage {
            id: "poll".into(),
            kind: "http_request".into(),
            payload: wire_payload(SurfaceEnvelope {
                surface: Some(ApiSurface::OpenAi),
                operation: Some(operation),
                method: "GET".into(),
                path: path.into(),
                has_query: !query.is_empty(),
                raw_query: query.into(),
                ..Default::default()
            })
            .unwrap(),
            output_credit: None,
        };
        let withdrawn = active.with_suppliers(vec![]);
        let status = request(ApiOperation::VideosGet, "/v1/videos/job-1", "");
        for config in [&active, &withdrawn] {
            assert_eq!(
                config
                    .supplier_for_request(&channel, &unit, &status, Some(&tasks))
                    .unwrap()
                    .upstream_base_url,
                "https://original.invalid"
            );
        }
        let new_status = request(ApiOperation::VideosGet, "/v1/videos/new-job", "");
        assert_eq!(
            active
                .supplier_for_request(&channel, &unit, &new_status, Some(&tasks))
                .unwrap()
                .upstream_base_url,
            "https://updated.invalid"
        );
        let content = request(
            ApiOperation::VideosContent,
            "/v1/videos/job-1/content",
            "variant=thumbnail",
        );
        assert_eq!(
            active
                .supplier_for_request(&channel, &unit, &content, Some(&tasks))
                .unwrap()
                .upstream_base_url,
            "https://original.invalid"
        );
        assert!(
            withdrawn
                .supplier_for_request(&channel, &unit, &content, Some(&tasks))
                .is_none()
        );
        let mut videos = tasks.accepted_videos();
        videos.prune(Instant::now() + Duration::from_secs(24 * 60 * 60));
        assert!(!videos.pending());
    }
}
