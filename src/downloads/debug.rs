// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `downloads` verbs of the debug channel (`dev/jctl downloads ...`).

use gpui_kit::{Context, Window};

use super::{engine, format_bytes, local_path};
use crate::{app::Bloom, jellyfin::Item};

impl Bloom {
    /// Where the player got its item: the local file or the stream, and
    /// the path mpv reports. For the debug channel.
    pub fn downloads_source_label(&self) -> String {
        let Some(playing) = self.playing.as_ref() else {
            return "none".to_string();
        };
        let server = self.session.as_ref().and_then(|s| s.client.server_id.clone());
        let source = match local_path(server.as_deref(), &playing.id) {
            Some(path) => format!("local:{}", path.display()),
            None => "stream".to_string(),
        };
        format!("{source} mpv_path={:?}", self.player_status.path)
    }

    /// The debug channel: `downloads list|add <id>|cancel <id>|resume <id>|
    /// remove <id>|remove-all|page|offline on|off|play <id>|state`.
    pub fn debug_downloads(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let (verb, arg) = rest.split_once(' ').unwrap_or((rest, ""));
        let arg = arg.trim();
        let engine = engine();
        match verb {
            "list" => {
                let mut lines: Vec<String> = engine
                    .entries()
                    .iter()
                    .map(|e| {
                        format!(
                            "{} | {} | {:?} | {}/{} | {}/s{}",
                            e.item_id,
                            e.title(),
                            e.state,
                            e.done,
                            e.total.map_or("?".to_string(), |t| t.to_string()),
                            format_bytes(e.speed as u64),
                            e.error.as_deref().map(|err| format!(" | {err}")).unwrap_or_default(),
                        )
                    })
                    .collect();
                if lines.is_empty() {
                    lines.push("no downloads".to_string());
                }
                return lines.join("\n");
            }
            "add" => {
                let id = arg.to_string();
                self.fetch(
                    cx,
                    move |client| client.item(&id),
                    |this, result, cx| match result {
                        Ok(item) => this.download_item(&item, cx),
                        Err(err) => log::warn!("debug downloads add: {err:#}"),
                    },
                );
            }
            "cancel" => engine.cancel(arg),
            "resume" => engine.resume(arg),
            "remove" => engine.remove(arg),
            "remove-all" => engine.remove_all(),
            "page" => self.open_downloads(cx),
            // The page of a downloaded item, from its `meta.json`.
            "open" => self.open_download(None, arg, cx),
            // The entries the card menu of an item gets, by their ids.
            "menu" => {
                let item = engine.item(arg).or_else(|| {
                    engine.entry(arg).map(|e| Item { id: e.item_id.clone(), kind: e.kind.as_str().into(), ..Default::default() })
                });
                let Some(item) = item else {
                    return "error: not a downloaded item; use `add` first".into();
                };
                let labels: Vec<String> = self
                    .download_menu_items(&item, cx)
                    .iter()
                    .filter_map(|entry| entry.debug_label())
                    .collect();
                return labels.join(" | ");
            }
            "offline" => match arg {
                "on" => {
                    self.simulate_unreachable(true);
                    engine.set_client(None, "");
                    self.open_home(cx);
                }
                "off" => self.retry_connection(cx),
                _ => return "error: downloads offline on|off".into(),
            },
            "play" => {
                self.muted = true;
                self.play_download(None, arg, window, cx);
            }
            "state" | "" => {}
            _ => {
                return "error: downloads list|add <id>|cancel <id>|resume <id>|remove <id>|remove-all|page|open <id>|menu <id>|offline on|off|play <id>|state".into();
            }
        }
        cx.notify();
        let all = engine.summary();
        format!(
            "downloads offline={} allowed={:?} owner={} entries={} active={} done={} used={} free={} dir={} source={} segments=[{}]",
            self.downloads.offline,
            self.downloads.allowed,
            engine.owns_storage(),
            all.entries,
            all.active,
            all.done,
            format_bytes(all.used),
            engine.free_bytes().map_or("?".to_string(), format_bytes),
            engine.dir().display(),
            self.downloads_source_label(),
            self.segments
                .iter()
                .map(|s| format!("{} {:.1}-{:.1}", s.kind, s.start_secs(), s.end_secs()))
                .collect::<Vec<_>>()
                .join(", "),
        )
    }
}
