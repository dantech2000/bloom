// Copyright (C) 2026 Sarat Chandra
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Remote image cache. The native GPUI platform ships without an HTTP client,
//! so artwork is fetched here and handed to `img()` as decoded-once bytes.

use std::{collections::HashMap, sync::Arc, time::Duration};

use gpui_kit::{App, Global, Image, ImageFormat};

enum Slot {
    Loading,
    Ready(Arc<Image>),
    Failed,
}

#[derive(Default)]
struct ImageStore {
    slots: HashMap<String, Slot>,
    agent: Option<ureq::Agent>,
}
impl Global for ImageStore {}

/// Returns the cached image for `url`, starting a download the first time.
/// Windows are refreshed when the image arrives.
pub fn image(url: &str, cx: &mut App) -> Option<Arc<Image>> {
    if !cx.has_global::<ImageStore>() {
        cx.set_global(ImageStore::default());
    }
    let store = cx.global_mut::<ImageStore>();
    match store.slots.get(url) {
        Some(Slot::Ready(image)) => return Some(image.clone()),
        Some(_) => return None,
        None => {}
    }
    store.slots.insert(url.to_string(), Slot::Loading);
    let agent = store
        .agent
        .get_or_insert_with(|| {
            ureq::Agent::new_with_config(
                ureq::Agent::config_builder()
                    .timeout_global(Some(Duration::from_secs(20)))
                    .build(),
            )
        })
        .clone();
    let key = url.to_string();
    let target = url.to_string();
    cx.spawn(async move |cx| {
        let fetched = cx
            .background_executor()
            .spawn(async move { download(&agent, &target) })
            .await;
        cx.update(|cx| {
            let slot = match fetched {
                Ok(image) => Slot::Ready(Arc::new(image)),
                Err(err) => {
                    log::warn!("image {key} failed: {err:#}");
                    Slot::Failed
                }
            };
            cx.global_mut::<ImageStore>().slots.insert(key, slot);
            cx.refresh_windows();
        });
    })
    .detach();
    None
}

fn download(agent: &ureq::Agent, url: &str) -> anyhow::Result<Image> {
    let mut response = agent.get(url).call()?;
    let mime = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(';').next().unwrap_or("").trim().to_string())
        .unwrap_or_default();
    let bytes = response.body_mut().read_to_vec()?;
    let format = ImageFormat::from_mime_type(&mime)
        .or_else(|| sniff(&bytes))
        .ok_or_else(|| anyhow::anyhow!("unknown image type {mime:?}"))?;
    Ok(Image::from_bytes(format, bytes))
}

fn sniff(bytes: &[u8]) -> Option<ImageFormat> {
    match bytes {
        [0xFF, 0xD8, ..] => Some(ImageFormat::Jpeg),
        [0x89, b'P', b'N', b'G', ..] => Some(ImageFormat::Png),
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => Some(ImageFormat::Webp),
        [b'G', b'I', b'F', ..] => Some(ImageFormat::Gif),
        _ => None,
    }
}
