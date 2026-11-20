---
name: const-api-image
description: Discover and use the user's configured CONST API for image generation and editing, video, narration, and music (生图、改图、生成视频、文字配音、配乐). Use for requests to deliver these media or check model availability.
---

Honor the user's chosen tool, provider, model, budget, and output. If a built-in media tool already uses the requested route and meets the need, use it directly. Some apps use a separate account or balance for native media even when chat uses CONST. To generate through CONST, use its native API binding or the executable and argument arrays below. Run with the tool's ordinary permissions, passing each argument separately and applying any listed environment only to the child process. The executable handles authentication.

Discover current models before choosing. Run `models` for the Images API and `media_models` for video, speech and native media operations. Use the exact model ID and operation together. For an inventory question, list confirmed operations first. Models in `declared_outputs` have a catalog description only unless an operation also lists them. Do not generate a paid sample to check availability.

Honor a named model. Otherwise select by requested capabilities and comparable prices, and identify the model used with the result. The order of IDs has no quality or price meaning. When supplied, `platform_pricing` lists reference USD rates, units and offer multipliers. Compare the same operation and units. A zero minimum may apply to only one offer. For a strict budget or free-only request use a route that can enforce it, or ask before generating.

**Images.** Choose from `generate` for a new image or `edit` for changes. Match the output format to the request. For a photo or other bitmap, use a model that can return PNG, JPEG or WebP with the image command. For an SVG, use a vector model through the Images API. For edits pass the original file to `--input`. If later edits must use the same model, select one in both lists. Use the corresponding arguments below with a fresh output path. Add `--size` and `--quality` only for supported options. Return the saved path printed by the command.

**Video.** Select from `video`. Run `video_create` once, sending a UTF-8 JSON object on standard input, for example `{"model":"MODEL","prompt":"PROMPT"}`. Include a duration or resolution only when the model supports it. Keep the returned ID. Poll `video_status` with that ID, spacing checks by several seconds. After `done` or `completed`, run `video_save` with the same ID and a new output filename. If a download is interrupted, retry saving the existing job. Report failed or expired jobs.

**Speech synthesis.** Select from `speech` and choose a voice supported by the model. Run `speech` with a new audio filename and a UTF-8 JSON object on standard input, for example `{"model":"MODEL","voice":"VOICE","input":"TEXT"}`. For a model in `speech_sse`, include `"stream_format":"sse"` so the platform receives final usage. The file extension selects the output format. The command saves binary or streamed audio and returns its path. Use this operation for spoken text.

**Native media.** Use the tool's configured CONST connection or SDK and the protocol named in `native_operations`. For `gemini.generate_content.image`, call Gemini `generateContent` with image output enabled and save the returned inline image. For edits, select a model listed under `gemini.generate_content.image_edit` and include the source image with its MIME type alongside the edit instructions. For `openai.chat_completions.music`, request audio output through Chat Completions and save the audio in its declared format. Respect the music model's supported durations. If the configured connection cannot call the required protocol, report the requirement.

Return the requested file, or report the job ID and status while a video is pending. If a paid submission may have been accepted and its outcome is unknown, ask before submitting again.
