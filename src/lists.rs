// SPDX-License-Identifier: AGPL-3.0-or-later
//! Playlists and collections: the calls to the server, the state of the
//! "Add to" dialog, the actions of the playlist page, and the `lists` debug
//! command. The dialog and the page are drawn in `views/playlist.rs`.

use std::rc::Rc;

use anyhow::{Result, anyhow, ensure};
use gpui_kit::{
    AppContext as _, Context, Entity, FocusHandle, Subscription, Window,
    base::input::{InputEvent, InputState},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    app::{Bloom, Page},
    jellyfin::{Client, Item, ItemQuery},
    ui::menu::MenuItem,
};

/// Id and collection type of the library page that lists the playlists.
pub const PLAYLISTS_ID: &str = "playlists";
/// A debug command changes only lists whose name starts with this.
pub const TEST_PREFIX: &str = "jellyui-test-";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ListKind {
    Playlist,
    Collection,
}

impl ListKind {
    pub fn noun(self) -> &'static str {
        match self {
            ListKind::Playlist => "playlist",
            ListKind::Collection => "collection",
        }
    }
}

// ----- Pure parts ---------------------------------------------------------

/// Only a playlist or a collection can be deleted. `DELETE /Items/{id}` also
/// deletes real media, so every delete goes through this check.
pub fn is_deletable_kind(kind: &str) -> bool {
    matches!(kind, "Playlist" | "BoxSet")
}

/// The kinds of item that can go into a playlist.
pub fn can_add_to_playlist(kind: &str) -> bool {
    matches!(kind, "Movie" | "Series" | "Season" | "Episode" | "Video")
}

/// The kinds of item that can go into a collection.
pub fn can_add_to_collection(kind: &str) -> bool {
    matches!(kind, "Movie" | "Series")
}

/// The place an entry takes when it moves one step. `place` counts from 0.
pub fn step_target(len: usize, place: usize, up: bool) -> Option<usize> {
    if place >= len {
        return None;
    }
    if up {
        place.checked_sub(1)
    } else {
        (place + 1 < len).then_some(place + 1)
    }
}

/// Moves one entry of a list to a new place, as the server does.
pub fn reorder<T>(list: &mut Vec<T>, from: usize, to: usize) {
    if from >= list.len() || to >= list.len() {
        return;
    }
    let entry = list.remove(from);
    list.insert(to, entry);
}

/// True for a name a debug command may change.
pub fn is_test_name(name: &str) -> bool {
    name.starts_with(TEST_PREFIX) && name.len() > TEST_PREFIX.len()
}

/// A request without a body: method, path and query.
#[derive(Debug, PartialEq)]
pub struct Req {
    pub method: &'static str,
    pub path: String,
    pub query: Vec<(&'static str, String)>,
}

fn ids_param(ids: &[String]) -> String {
    ids.join(",")
}

pub fn add_to_playlist_req(list: &str, ids: &[String], user: &str) -> Req {
    Req {
        method: "POST",
        path: format!("/Playlists/{list}/Items"),
        query: vec![("ids", ids_param(ids)), ("userId", user.to_string())],
    }
}

pub fn remove_from_playlist_req(list: &str, entry_ids: &[String]) -> Req {
    Req {
        method: "DELETE",
        path: format!("/Playlists/{list}/Items"),
        query: vec![("entryIds", ids_param(entry_ids))],
    }
}

pub fn move_in_playlist_req(list: &str, entry_id: &str, new_index: usize) -> Req {
    Req {
        method: "POST",
        path: format!("/Playlists/{list}/Items/{entry_id}/Move/{new_index}"),
        query: Vec::new(),
    }
}

pub fn create_collection_req(name: &str, ids: &[String]) -> Req {
    Req {
        method: "POST",
        path: "/Collections".to_string(),
        query: vec![("name", name.to_string()), ("ids", ids_param(ids))],
    }
}

pub fn add_to_collection_req(list: &str, ids: &[String]) -> Req {
    Req {
        method: "POST",
        path: format!("/Collections/{list}/Items"),
        query: vec![("ids", ids_param(ids))],
    }
}

pub fn remove_from_collection_req(list: &str, ids: &[String]) -> Req {
    Req {
        method: "DELETE",
        path: format!("/Collections/{list}/Items"),
        query: vec![("ids", ids_param(ids))],
    }
}

pub fn create_playlist_body(name: &str, ids: &[String], user: &str) -> Value {
    json!({ "Name": name, "Ids": ids, "UserId": user, "MediaType": "Video" })
}

pub fn rename_playlist_body(name: &str) -> Value {
    json!({ "Name": name })
}

// ----- Calls to the server ------------------------------------------------

/// An entry of a playlist: the item, and the id of its place in the list
/// (the same item can be in a playlist twice).
#[derive(Clone, Debug, Deserialize)]
pub struct Entry {
    #[serde(rename = "PlaylistItemId", default)]
    pub entry_id: String,
    #[serde(flatten)]
    pub item: Item,
}

#[derive(Deserialize)]
struct Entries {
    #[serde(rename = "Items", default)]
    items: Vec<Entry>,
}

#[derive(Deserialize)]
struct Created {
    #[serde(rename = "Id", default)]
    id: String,
}

impl Client {
    fn list_query(&self, kind: &str) -> Result<Vec<Item>> {
        let user = self.user()?.to_string();
        let result: crate::jellyfin::ItemsResult = self.get(
            "/Items",
            &[
                ("userId", user),
                ("includeItemTypes", kind.to_string()),
                ("recursive", "true".to_string()),
                ("sortBy", "SortName".to_string()),
                ("fields", "ChildCount,PrimaryImageAspectRatio".to_string()),
                ("enableImageTypes", "Primary".to_string()),
            ],
        )?;
        Ok(result.items)
    }

    pub fn playlists(&self) -> Result<Vec<Item>> {
        self.list_query("Playlist")
    }

    pub fn collections(&self) -> Result<Vec<Item>> {
        self.list_query("BoxSet")
    }

    pub fn playlist_entries(&self, list: &str) -> Result<Vec<Entry>> {
        let user = self.user()?.to_string();
        let entries: Entries = self.get(
            &format!("/Playlists/{list}/Items"),
            &[
                ("userId", user),
                ("fields", "PrimaryImageAspectRatio,ChildCount".to_string()),
                ("enableImageTypes", "Primary,Backdrop,Thumb".to_string()),
            ],
        )?;
        Ok(entries.items)
    }

    fn run(&self, req: Req) -> Result<()> {
        self.call(req.method, &req.path, &req.query)
    }

    pub fn create_playlist(&self, name: &str, ids: &[String]) -> Result<String> {
        let user = self.user()?.to_string();
        let mut body = self.post("/Playlists", &create_playlist_body(name, ids, &user))?;
        let created: Created = body.read_json()?;
        ensure!(!created.id.is_empty(), "the server sent no id for the playlist");
        Ok(created.id)
    }

    pub fn add_to_playlist(&self, list: &str, ids: &[String]) -> Result<()> {
        let user = self.user()?.to_string();
        self.run(add_to_playlist_req(list, ids, &user))
    }

    pub fn remove_from_playlist(&self, list: &str, entry_ids: &[String]) -> Result<()> {
        self.run(remove_from_playlist_req(list, entry_ids))
    }

    pub fn move_in_playlist(&self, list: &str, entry_id: &str, new_index: usize) -> Result<()> {
        self.run(move_in_playlist_req(list, entry_id, new_index))
    }

    pub fn rename_playlist(&self, list: &str, name: &str) -> Result<()> {
        self.post(&format!("/Playlists/{list}"), &rename_playlist_body(name))?;
        Ok(())
    }

    pub fn create_collection(&self, name: &str, ids: &[String]) -> Result<String> {
        let req = create_collection_req(name, ids);
        let created: Created = self.send(req.method, &req.path, &req.query)?;
        ensure!(!created.id.is_empty(), "the server sent no id for the collection");
        Ok(created.id)
    }

    pub fn add_to_collection(&self, list: &str, ids: &[String]) -> Result<()> {
        self.run(add_to_collection_req(list, ids))
    }

    pub fn remove_from_collection(&self, list: &str, ids: &[String]) -> Result<()> {
        self.run(remove_from_collection_req(list, ids))
    }

    /// Deletes a playlist or a collection, and nothing else. The server
    /// says what the item is; the caller's word for it is not trusted.
    pub fn delete_list(&self, id: &str) -> Result<()> {
        let item = self.item(id)?;
        ensure!(
            is_deletable_kind(&item.kind),
            "refused to delete {:?}: it is a {}, not a playlist or a collection",
            item.name,
            item.kind
        );
        self.call("DELETE", &format!("/Items/{id}"), &[])
    }

    /// Whether the user may make collections.
    pub fn can_collect(&self) -> Result<bool> {
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Me {
            #[serde(default)]
            policy: Policy,
        }
        #[derive(Default, Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Policy {
            #[serde(default)]
            is_administrator: bool,
            #[serde(default)]
            enable_collection_management: bool,
        }
        let me: Me = self.get("/Users/Me", &[])?;
        Ok(me.policy.is_administrator || me.policy.enable_collection_management)
    }
}

// ----- State ----------------------------------------------------------------

/// A question or form over the page.
#[derive(Clone)]
pub enum Dialog {
    /// Pick a list for an item, or make a new one. `lists` is `None` while
    /// the server answers.
    Add {
        kind: ListKind,
        item: Item,
        lists: Option<Vec<Item>>,
    },
    Rename {
        list: Item,
    },
    Confirm {
        title: String,
        message: String,
        action: String,
        run: Rc<dyn Fn(&mut Bloom, &mut Context<Bloom>)>,
    },
}

pub struct State {
    /// The playlists of the user; the navigation shows "Playlists" only
    /// when there is one.
    pub playlists: Vec<Item>,
    /// The user may make collections.
    pub can_collect: bool,
    pub dialog: Option<Dialog>,
    /// The name field of the dialog.
    pub input: Entity<InputState>,
    pub focus: FocusHandle,
    /// The dialog opened since the last render: set the field and focus.
    fresh: bool,
    _subscription: Subscription,
}

impl State {
    pub fn new(window: &mut Window, cx: &mut Context<Bloom>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Name"));
        let subscription = cx.subscribe_in(&input, window, |this, _, event, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.submit_lists_dialog(window, cx);
            }
        });
        Self {
            playlists: Vec::new(),
            can_collect: false,
            dialog: None,
            input,
            focus: cx.focus_handle(),
            fresh: false,
            _subscription: subscription,
        }
    }
}

/// What the page of one playlist shows.
#[derive(Default)]
pub struct PlaylistData {
    pub list: Item,
    pub entries: Vec<Entry>,
    pub loading: bool,
}

impl Bloom {
    /// Loads the playlists and the collection right of the user.
    pub fn load_lists(&mut self, cx: &mut Context<Self>) {
        self.fetch(
            cx,
            |client| Ok((client.playlists()?, client.can_collect().unwrap_or(false))),
            |this, result, cx| {
                match result {
                    Ok((playlists, can_collect)) => {
                        this.lists.playlists = playlists;
                        this.lists.can_collect = can_collect;
                    }
                    Err(err) => log::warn!("lists: could not load the playlists: {err:#}"),
                }
                cx.notify();
            },
        );
    }

    /// Opens the page with the playlists as cards.
    pub fn open_playlists(&mut self, cx: &mut Context<Self>) {
        self.open_library(
            Item {
                id: PLAYLISTS_ID.into(),
                name: "Playlists".into(),
                collection_type: Some(PLAYLISTS_ID.into()),
                ..Default::default()
            },
            cx,
        );
    }

    pub fn open_playlist(&mut self, list: Item, cx: &mut Context<Self>) {
        self.navigate(
            Page::Playlist(PlaylistData {
                list,
                entries: Vec::new(),
                loading: true,
            }),
            cx,
        );
    }

    /// Loads the entries of the playlist on the page.
    pub fn load_playlist(&mut self, generation: u64, cx: &mut Context<Self>) {
        let Page::Playlist(data) = &mut self.page else {
            return;
        };
        data.loading = true;
        let id = data.list.id.clone();
        self.fetch(
            cx,
            move |client| Ok((client.item(&id)?, client.playlist_entries(&id)?)),
            move |this, result, cx| {
                if this.generation != generation {
                    return;
                }
                if let Page::Playlist(data) = &mut this.page {
                    data.loading = false;
                    match result {
                        Ok((list, entries)) => {
                            data.list = list;
                            data.entries = entries;
                        }
                        Err(err) => this.toast("Could not load the playlist", format!("{err:#}"), cx),
                    }
                }
                cx.notify();
            },
        );
    }

    // ----- Menu entries -----------------------------------------------------

    /// "Add to playlist" and "Add to collection" for an item. Empty for a
    /// kind of item no list takes.
    pub fn list_menu_items(&self, item: &Item, cx: &mut Context<Self>) -> Vec<MenuItem> {
        let this = cx.weak_entity();
        let mut items = Vec::new();
        let mut entry = |id: &'static str, label: &'static str, kind: ListKind| {
            let (handle, target) = (this.clone(), item.clone());
            items.push(MenuItem::new(id, label).on_click(move |_, window, cx| {
                handle
                    .update(cx, |this, cx| {
                        this.open_add_dialog(kind, target.clone(), window, cx)
                    })
                    .ok();
            }));
        };
        if can_add_to_playlist(&item.kind) {
            entry("lists.menu.playlist", "Add to playlist", ListKind::Playlist);
        }
        if can_add_to_collection(&item.kind) && self.lists.can_collect {
            entry(
                "lists.menu.collection",
                "Add to collection",
                ListKind::Collection,
            );
        }
        items
    }

    /// "Remove from collection" for a card on the page of a collection.
    pub fn collection_card_items(&self, item: &Item, cx: &mut Context<Self>) -> Vec<MenuItem> {
        let Page::Library(data) = &self.page else {
            return Vec::new();
        };
        if data.view.kind != "BoxSet" || !self.lists.can_collect {
            return Vec::new();
        }
        let (handle, collection, target) = (cx.weak_entity(), data.view.clone(), item.clone());
        vec![
            MenuItem::new("lists.menu.uncollect", "Remove from collection").on_click(
                move |_, _, cx| {
                    handle
                        .update(cx, |this, cx| {
                            this.remove_from_collection(collection.clone(), target.clone(), cx)
                        })
                        .ok();
                },
            ),
        ]
    }

    /// The menu of the page of a collection: delete it. Empty for any
    /// other page.
    pub fn rebuild_collection_menu(&mut self, cx: &mut Context<Self>) {
        let Page::Library(data) = &self.page else {
            return;
        };
        if data.view.kind != "BoxSet" {
            return;
        }
        let (handle, collection) = (cx.weak_entity(), data.view.clone());
        let mut items = vec![MenuItem::new("lists.menu.refresh", "Refresh").on_click({
            let handle = handle.clone();
            move |_, _, cx| {
                handle.update(cx, |this, cx| this.load_page(cx)).ok();
            }
        })];
        if self.lists.can_collect {
            items.push(MenuItem::separator());
            items.push(
                MenuItem::new("lists.menu.delete", "Delete collection").on_click(
                    move |_, window, cx| {
                        handle
                            .update(cx, |this, cx| {
                                this.ask_delete_list(collection.clone(), window, cx)
                            })
                            .ok();
                    },
                ),
            );
        }
        self.more_menu.update(cx, |menu, cx| menu.set_items(items, cx));
    }

    // ----- The dialog -------------------------------------------------------

    fn open_lists_dialog(&mut self, dialog: Dialog, window: &mut Window, cx: &mut Context<Self>) {
        self.close_popups(None, window, cx);
        self.lists.dialog = Some(dialog);
        self.lists.fresh = true;
        cx.notify();
    }

    pub fn close_lists_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.lists.dialog.take().is_some() {
            self.lists
                .input
                .update(cx, |input, cx| input.set_value("", window, cx));
            window.focus(&self.app_focus, cx);
            cx.notify();
        }
    }

    /// Fills the name field and takes the focus, for a dialog that just
    /// opened. The root render calls this.
    pub fn prepare_lists(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.lists.fresh) {
            return;
        }
        let Some(dialog) = self.lists.dialog.clone() else {
            return;
        };
        let (value, placeholder) = match &dialog {
            Dialog::Add { kind, .. } => (String::new(), format!("New {} name", kind.noun())),
            Dialog::Rename { list } => (list.name.clone(), "Name".to_string()),
            Dialog::Confirm { .. } => (String::new(), String::new()),
        };
        self.lists.input.update(cx, |input, cx| {
            input.set_value(value, window, cx);
            input.set_placeholder(placeholder, window, cx);
        });
        match dialog {
            Dialog::Confirm { .. } => window.focus(&self.lists.focus, cx),
            _ => {
                let focus = gpui_kit::Focusable::focus_handle(self.lists.input.read(cx), cx);
                window.focus(&focus, cx);
            }
        }
    }

    pub fn open_add_dialog(
        &mut self,
        kind: ListKind,
        item: Item,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_lists_dialog(
            Dialog::Add {
                kind,
                item,
                lists: None,
            },
            window,
            cx,
        );
        self.fetch(
            cx,
            move |client| match kind {
                ListKind::Playlist => client.playlists(),
                ListKind::Collection => client.collections(),
            },
            move |this, result, cx| {
                let lists = match result {
                    Ok(lists) => lists,
                    Err(err) => {
                        this.toast("Could not load the lists", format!("{err:#}"), cx);
                        Vec::new()
                    }
                };
                if let Some(Dialog::Add { lists: slot, .. }) = &mut this.lists.dialog {
                    *slot = Some(lists);
                }
                cx.notify();
            },
        );
    }

    /// Enter in the name field, or the main button of the dialog.
    pub fn submit_lists_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.lists.dialog.clone() else {
            return;
        };
        let name = self.lists.input.read(cx).value().trim().to_string();
        match dialog {
            Dialog::Add { kind, item, .. } => {
                if name.is_empty() {
                    return;
                }
                self.close_lists_dialog(window, cx);
                self.create_list(kind, name, item, cx);
            }
            Dialog::Rename { list } => {
                if name.is_empty() {
                    return;
                }
                self.close_lists_dialog(window, cx);
                self.rename_playlist(list, name, cx);
            }
            Dialog::Confirm { run, .. } => {
                self.close_lists_dialog(window, cx);
                run(self, cx);
            }
        }
    }

    // ----- Actions ------------------------------------------------------------

    /// Runs a change on the server. On success it shows `ok` as a toast,
    /// loads the lists and the page again, and runs `then`.
    fn list_change<W>(
        &mut self,
        ok: String,
        failed: &'static str,
        work: W,
        then: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) where
        W: FnOnce(Client) -> Result<()> + Send + 'static,
    {
        self.fetch(cx, work, move |this, result, cx| match result {
            Ok(()) => {
                this.toast(ok, "", cx);
                this.after_list_change(cx);
                then(this, cx);
            }
            Err(err) => this.toast(failed, format!("{err:#}"), cx),
        });
    }

    /// A playlist or collection appeared, changed or went away: the lists
    /// of the navigation, the title index and the page load again.
    pub fn after_list_change(&mut self, cx: &mut Context<Self>) {
        self.load_lists(cx);
        self.reload_catalog(cx);
        if matches!(self.page, Page::Library(_) | Page::Playlist(_)) {
            self.load_page(cx);
        }
        cx.notify();
    }

    /// Puts an item into a list the user picked in the dialog.
    pub fn add_to_list(
        &mut self,
        kind: ListKind,
        list: Item,
        item: Item,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_lists_dialog(window, cx);
        let ok = format!("Added to {}", list.name);
        let (list_id, item_id) = (list.id, item.id);
        self.list_change(
            ok,
            "Could not add the item",
            move |client| {
                let ids = [item_id];
                match kind {
                    ListKind::Playlist => client.add_to_playlist(&list_id, &ids),
                    ListKind::Collection => client.add_to_collection(&list_id, &ids),
                }
            },
            |_, _| {},
            cx,
        );
    }

    /// Makes a list with the item in it.
    pub fn create_list(&mut self, kind: ListKind, name: String, item: Item, cx: &mut Context<Self>) {
        let ok = format!("Made the {} {name}", kind.noun());
        let item_id = item.id;
        self.list_change(
            ok,
            "Could not make the list",
            move |client| {
                let ids = [item_id];
                match kind {
                    ListKind::Playlist => client.create_playlist(&name, &ids).map(|_| ()),
                    ListKind::Collection => client.create_collection(&name, &ids).map(|_| ()),
                }
            },
            |_, _| {},
            cx,
        );
    }

    pub fn rename_playlist(&mut self, list: Item, name: String, cx: &mut Context<Self>) {
        let ok = format!("Renamed to {name}");
        self.list_change(
            ok,
            "Could not rename the playlist",
            move |client| client.rename_playlist(&list.id, &name),
            |_, _| {},
            cx,
        );
    }

    pub fn ask_rename_playlist(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Page::Playlist(data) = &self.page else {
            return;
        };
        let list = data.list.clone();
        self.open_lists_dialog(Dialog::Rename { list }, window, cx);
    }

    /// Asks before a playlist or a collection is deleted. Only these two
    /// kinds are offered; `delete_list` of the client checks again.
    pub fn ask_delete_list(&mut self, list: Item, window: &mut Window, cx: &mut Context<Self>) {
        if !is_deletable_kind(&list.kind) {
            log::warn!("lists: refused to offer delete for a {}", list.kind);
            return;
        }
        let noun = if list.kind == "Playlist" { "playlist" } else { "collection" };
        let message = format!("\"{}\" goes away. The titles in it stay in the library.", list.name);
        let target = list.clone();
        self.open_lists_dialog(
            Dialog::Confirm {
                title: format!("Delete {noun}?"),
                message,
                action: "Delete".into(),
                run: Rc::new(move |this, cx| this.delete_list(target.clone(), cx)),
            },
            window,
            cx,
        );
    }

    pub fn delete_list(&mut self, list: Item, cx: &mut Context<Self>) {
        if !is_deletable_kind(&list.kind) {
            self.toast("Could not delete", format!("{} is not a playlist or a collection.", list.name), cx);
            return;
        }
        let ok = format!("Deleted {}", list.name);
        let id = list.id.clone();
        // The page of the list is gone with it: go back and load again.
        let on_list_page = match &self.page {
            Page::Playlist(data) => data.list.id == list.id,
            Page::Library(data) => data.view.id == list.id,
            _ => false,
        };
        self.list_change(
            ok,
            "Could not delete",
            move |client| client.delete_list(&id),
            move |this, cx| {
                if on_list_page {
                    if this.history.is_empty() {
                        this.open_home(cx);
                    } else {
                        this.back(cx);
                        this.load_page(cx);
                    }
                }
            },
            cx,
        );
    }

    pub fn remove_from_collection(&mut self, collection: Item, item: Item, cx: &mut Context<Self>) {
        let ok = format!("Removed {} from {}", item.name, collection.name);
        let (id, item_id) = (collection.id, item.id);
        self.list_change(
            ok,
            "Could not remove the item",
            move |client| client.remove_from_collection(&id, &[item_id]),
            |_, _| {},
            cx,
        );
    }

    /// Takes the entry at a place (from 0) out of the playlist on the page.
    pub fn playlist_remove(&mut self, place: usize, cx: &mut Context<Self>) {
        let Page::Playlist(data) = &self.page else {
            return;
        };
        let Some(entry) = data.entries.get(place) else {
            return;
        };
        let (id, entry_id, title) = (data.list.id.clone(), entry.entry_id.clone(), entry.item.display_title());
        self.list_change(
            format!("Removed {title}"),
            "Could not remove the entry",
            move |client| client.remove_from_playlist(&id, &[entry_id]),
            |_, _| {},
            cx,
        );
    }

    /// Moves the entry at a place (from 0) to a new place.
    pub fn playlist_move(&mut self, place: usize, to: usize, cx: &mut Context<Self>) {
        let Page::Playlist(data) = &mut self.page else {
            return;
        };
        let Some(entry) = data.entries.get(place) else {
            return;
        };
        if to >= data.entries.len() || to == place {
            return;
        }
        let (id, entry_id) = (data.list.id.clone(), entry.entry_id.clone());
        // The page shows the new order at once; the server answer confirms it.
        reorder(&mut data.entries, place, to);
        cx.notify();
        self.list_change(
            "Moved".into(),
            "Could not move the entry",
            move |client| client.move_in_playlist(&id, &entry_id, to),
            |_, _| {},
            cx,
        );
    }

    /// Plays the playlist on the page from an entry (from 0), or all of it
    /// in random order.
    pub fn play_playlist(&mut self, from: usize, shuffle: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Page::Playlist(data) = &self.page else {
            return;
        };
        let mut items: Vec<Item> = data.entries.iter().skip(from).map(|e| e.item.clone()).collect();
        if items.is_empty() {
            self.toast("Nothing to play", "The playlist is empty.", cx);
            return;
        }
        if shuffle {
            use std::hash::{BuildHasher as _, Hasher as _};
            items.sort_by_cached_key(|_| std::collections::hash_map::RandomState::new().build_hasher().finish());
        }
        self.start_queue(items, window, cx);
    }

    // ----- Debug ----------------------------------------------------------------

    /// The `lists` debug command. It calls the server on the UI thread,
    /// which is fine for a test tool. Verbs that change a list work on lists
    /// named `jellyui-test-...` only.
    pub fn debug_lists(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> String {
        let Some(client) = self.session.as_ref().map(|s| s.client.clone()) else {
            return "error: not signed in".into();
        };
        let mut words = rest.split_whitespace();
        let verb = words.next().unwrap_or("");
        let args: Vec<&str> = words.collect();
        let result = self.debug_lists_verb(&client, verb, &args, rest, window, cx);
        match result {
            Ok(text) => text,
            Err(err) => format!("error: {err:#}"),
        }
    }

    fn debug_lists_verb(
        &mut self,
        client: &Client,
        verb: &str,
        args: &[&str],
        rest: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<String> {
        let line = |item: &Item, count: usize| format!("{} {:?} {}", item.id, item.name, count);
        let count_of = |client: &Client, item: &Item| -> usize {
            if item.kind == "Playlist" {
                client.playlist_entries(&item.id).map(|e| e.len()).unwrap_or(0)
            } else {
                item.child_count.unwrap_or(0).max(0) as usize
            }
        };
        // The test name of a list, or an error.
        let guard = |client: &Client, id: &str| -> Result<Item> {
            let item = client.item(id)?;
            ensure!(
                is_test_name(&item.name) && is_deletable_kind(&item.kind),
                "refused: {:?} is not a list named {TEST_PREFIX}..."
                , item.name
            );
            Ok(item)
        };
        let ids_of = |words: &[&str]| -> Vec<String> {
            words.iter().flat_map(|w| w.split(',')).filter(|w| !w.is_empty()).map(String::from).collect()
        };
        let place = |text: Option<&&str>| -> Result<usize> {
            let n: usize = text.ok_or_else(|| anyhow!("a place is missing"))?.parse()?;
            ensure!(n >= 1, "places count from 1");
            Ok(n - 1)
        };
        // The text after `n` words, for names with spaces.
        let tail = |n: usize| rest.split_whitespace().skip(n).collect::<Vec<_>>().join(" ");
        match verb {
            "playlists" | "collections" => {
                let lists = if verb == "playlists" { client.playlists()? } else { client.collections()? };
                Ok(lists.iter().map(|l| line(l, count_of(client, l))).collect::<Vec<_>>().join(" | "))
            }
            "new-playlist" | "new-collection" => {
                let name = args.first().ok_or_else(|| anyhow!("a name is missing"))?;
                ensure!(is_test_name(name), "refused: the name must start with {TEST_PREFIX}");
                let ids = ids_of(&args[1..]);
                let id = if verb == "new-playlist" {
                    client.create_playlist(name, &ids)?
                } else {
                    client.create_collection(name, &ids)?
                };
                self.after_list_change(cx);
                Ok(id)
            }
            "add" => {
                let list = guard(client, args.first().ok_or_else(|| anyhow!("a list id is missing"))?)?;
                let ids = ids_of(&args[1..]);
                ensure!(!ids.is_empty(), "an item id is missing");
                if list.kind == "Playlist" {
                    client.add_to_playlist(&list.id, &ids)?;
                } else {
                    client.add_to_collection(&list.id, &ids)?;
                }
                self.after_list_change(cx);
                Ok("added".into())
            }
            "entries" => {
                let id = args.first().ok_or_else(|| anyhow!("a list id is missing"))?;
                let list = client.item(id)?;
                if list.kind == "Playlist" {
                    let entries = client.playlist_entries(id)?;
                    Ok(entries
                        .iter()
                        .enumerate()
                        .map(|(n, e)| format!("{} {} {:?} entry={}", n + 1, e.item.id, e.item.display_title(), e.entry_id))
                        .collect::<Vec<_>>()
                        .join(" | "))
                } else {
                    let page = client.items(&ItemQuery { parent_id: Some(list.id), ..Default::default() })?;
                    Ok(page
                        .items
                        .iter()
                        .enumerate()
                        .map(|(n, i)| format!("{} {} {:?}", n + 1, i.id, i.name))
                        .collect::<Vec<_>>()
                        .join(" | "))
                }
            }
            "remove" => {
                let list = guard(client, args.first().ok_or_else(|| anyhow!("a list id is missing"))?)?;
                let n = place(args.get(1))?;
                if list.kind == "Playlist" {
                    let entries = client.playlist_entries(&list.id)?;
                    let entry = entries.get(n).ok_or_else(|| anyhow!("no entry at that place"))?;
                    client.remove_from_playlist(&list.id, &[entry.entry_id.clone()])?;
                } else {
                    let page = client.items(&ItemQuery { parent_id: Some(list.id.clone()), ..Default::default() })?;
                    let item = page.items.get(n).ok_or_else(|| anyhow!("no item at that place"))?;
                    client.remove_from_collection(&list.id, &[item.id.clone()])?;
                }
                self.after_list_change(cx);
                Ok("removed".into())
            }
            "move" => {
                let list = guard(client, args.first().ok_or_else(|| anyhow!("a list id is missing"))?)?;
                ensure!(list.kind == "Playlist", "only a playlist has an order");
                let (from, to) = (place(args.get(1))?, place(args.get(2))?);
                let entries = client.playlist_entries(&list.id)?;
                let entry = entries.get(from).ok_or_else(|| anyhow!("no entry at that place"))?;
                ensure!(to < entries.len(), "no such target place");
                client.move_in_playlist(&list.id, &entry.entry_id, to)?;
                self.after_list_change(cx);
                Ok("moved".into())
            }
            "rename" => {
                let list = guard(client, args.first().ok_or_else(|| anyhow!("a list id is missing"))?)?;
                let name = tail(2);
                ensure!(list.kind == "Playlist", "only a playlist is renamed here");
                ensure!(is_test_name(&name), "refused: the new name must start with {TEST_PREFIX}");
                client.rename_playlist(&list.id, &name)?;
                self.after_list_change(cx);
                Ok("renamed".into())
            }
            "delete" => {
                let list = guard(client, args.first().ok_or_else(|| anyhow!("a list id is missing"))?)?;
                client.delete_list(&list.id)?;
                // A page of the deleted list goes back.
                let gone = match &self.page {
                    Page::Playlist(d) => d.list.id == list.id,
                    Page::Library(d) => d.view.id == list.id,
                    _ => false,
                };
                if gone {
                    self.back(cx);
                }
                self.after_list_change(cx);
                Ok("deleted".into())
            }
            // The page of a playlist, or of a collection.
            "page" => {
                let id = args.first().ok_or_else(|| anyhow!("a list id is missing"))?;
                let item = client.item(id)?;
                self.open_item(item, cx);
                Ok("opened".into())
            }
            "nav" => {
                self.load_lists(cx);
                Ok(format!(
                    "playlists={} collect={}",
                    self.lists.playlists.len(),
                    self.lists.can_collect
                ))
            }
            "playlists-page" => {
                self.open_playlists(cx);
                Ok("opened".into())
            }
            // The dialogs, as the menu entries open them.
            "dialog" => {
                let kind = match args.first() {
                    Some(&"playlist") => ListKind::Playlist,
                    Some(&"collection") => ListKind::Collection,
                    _ => return Err(anyhow!("dialog <playlist|collection> <item id>")),
                };
                let item = client.item(args.get(1).ok_or_else(|| anyhow!("an item id is missing"))?)?;
                self.open_add_dialog(kind, item, window, cx);
                Ok("opened".into())
            }
            "dialog-state" => Ok(match &self.lists.dialog {
                None => "closed".into(),
                Some(Dialog::Add { kind, item, lists }) => format!(
                    "add {} item={:?} lists={}",
                    kind.noun(),
                    item.name,
                    lists.as_ref().map_or("loading".into(), |l| l.iter().map(|i| format!("{:?}", i.name)).collect::<Vec<_>>().join(","))
                ),
                Some(Dialog::Rename { list }) => format!("rename {:?}", list.name),
                Some(Dialog::Confirm { title, .. }) => format!("confirm {title:?}"),
            }),
            // Picks a row of the open dialog, as a click does (from 1).
            "dialog-pick" => {
                let Some(Dialog::Add { kind, item, lists: Some(lists) }) = self.lists.dialog.clone() else {
                    return Err(anyhow!("no dialog with a list"));
                };
                let list = lists.get(place(args.first())?).ok_or_else(|| anyhow!("no such row"))?.clone();
                ensure!(is_test_name(&list.name), "refused: not a {TEST_PREFIX} list");
                self.add_to_list(kind, list, item, window, cx);
                Ok("picked".into())
            }
            // Types the text in the name field and presses Enter.
            "dialog-name" => {
                let name = tail(1);
                ensure!(is_test_name(&name), "refused: the name must start with {TEST_PREFIX}");
                self.lists.input.update(cx, |input, cx| input.set_value(name, window, cx));
                self.submit_lists_dialog(window, cx);
                Ok("submitted".into())
            }
            "dialog-cancel" => {
                self.close_lists_dialog(window, cx);
                Ok("closed".into())
            }
            // The buttons of the playlist page (a place counts from 1).
            "page-state" => Ok(match &self.page {
                Page::Playlist(d) => format!(
                    "playlist {:?} loading={} entries={}",
                    d.list.name,
                    d.loading,
                    d.entries.len()
                ),
                Page::Library(d) => format!("library {:?} kind={} items={}", d.view.name, d.view.kind, d.items.len()),
                _ => "other page".into(),
            }),
            "page-up" | "page-down" => {
                let Page::Playlist(data) = &self.page else {
                    return Err(anyhow!("not on a playlist page"));
                };
                ensure!(is_test_name(&data.list.name), "refused: not a {TEST_PREFIX} list");
                let n = place(args.first())?;
                let to = step_target(data.entries.len(), n, verb == "page-up")
                    .ok_or_else(|| anyhow!("cannot move there"))?;
                self.playlist_move(n, to, cx);
                Ok("moving".into())
            }
            "page-remove" => {
                let n = place(args.first())?;
                match &self.page {
                    Page::Playlist(data) => {
                        ensure!(is_test_name(&data.list.name), "refused: not a {TEST_PREFIX} list");
                        self.playlist_remove(n, cx);
                    }
                    // On the page of a collection: the menu entry "Remove from collection".
                    Page::Library(data) if data.view.kind == "BoxSet" => {
                        ensure!(is_test_name(&data.view.name), "refused: not a {TEST_PREFIX} list");
                        let item = data.items.get(n).ok_or_else(|| anyhow!("no item at that place"))?.clone();
                        let collection = data.view.clone();
                        self.remove_from_collection(collection, item, cx);
                    }
                    _ => return Err(anyhow!("not on a playlist or collection page")),
                }
                Ok("removing".into())
            }
            // Opens the menu of a card of the library page, at the top left.
            "card-menu" => {
                let Page::Library(data) = &self.page else {
                    return Err(anyhow!("not on a library page"));
                };
                let item = data.items.get(place(args.first())?).ok_or_else(|| anyhow!("no item at that place"))?.clone();
                let at = gpui_kit::point(gpui_kit::px(400.), gpui_kit::px(200.));
                self.open_card_menu(&item, crate::app::CardRow::None, at, window, cx);
                Ok("opened".into())
            }
            "menu-more" => {
                let menu = self.more_menu.clone();
                menu.update(cx, |menu, cx| menu.open(window, cx));
                Ok("opened".into())
            }
            "page-rename" => {
                self.ask_rename_playlist(window, cx);
                Ok("dialog open; use dialog-name <name>".into())
            }
            // Opens the delete question of the page; `dialog-confirm` runs it.
            "page-delete" => {
                let list = match &self.page {
                    Page::Playlist(d) => d.list.clone(),
                    Page::Library(d) => d.view.clone(),
                    _ => return Err(anyhow!("not on a list page")),
                };
                ensure!(is_test_name(&list.name), "refused: not a {TEST_PREFIX} list");
                self.ask_delete_list(list, window, cx);
                Ok("dialog open; use dialog-confirm".into())
            }
            "dialog-confirm" => {
                ensure!(matches!(self.lists.dialog, Some(Dialog::Confirm { .. })), "no question is open");
                self.submit_lists_dialog(window, cx);
                Ok("confirmed".into())
            }
            "playall" => {
                // A test never plays aloud.
                self.muted = true;
                self.play_playlist(0, args.first() == Some(&"shuffle"), window, cx);
                Ok("playing".into())
            }
            _ => Err(anyhow!(
                "lists playlists|collections|new-playlist <name> <ids>|new-collection <name> <ids>|add <list> <item>|\
                 entries <list>|remove <list> <place>|move <list> <from> <to>|rename <list> <name>|delete <list>|\
                 dialog <playlist|collection> <item>|dialog-state|dialog-pick <n>|dialog-name <name>|dialog-cancel|\
                 dialog-confirm|page <list>|page-state|page-up <n>|page-down <n>|page-remove <n>|page-rename|\
                 page-delete|playall [shuffle]|playlists-page|nav"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn only_a_playlist_or_a_collection_is_deletable() {
        assert!(is_deletable_kind("Playlist"));
        assert!(is_deletable_kind("BoxSet"));
        for kind in ["Movie", "Series", "Season", "Episode", "Folder", "CollectionFolder", "UserView", ""] {
            assert!(!is_deletable_kind(kind), "{kind}");
        }
    }

    #[test]
    fn test_names() {
        assert!(is_test_name("jellyui-test-playlist"));
        assert!(!is_test_name("jellyui-test-"));
        assert!(!is_test_name("A Real Collection"));
    }

    #[test]
    fn step_targets() {
        assert_eq!(step_target(3, 0, true), None);
        assert_eq!(step_target(3, 1, true), Some(0));
        assert_eq!(step_target(3, 1, false), Some(2));
        assert_eq!(step_target(3, 2, false), None);
        assert_eq!(step_target(3, 3, true), None);
        assert_eq!(step_target(0, 0, false), None);
    }

    #[test]
    fn reorder_moves_one_entry() {
        let mut list = vec!['a', 'b', 'c', 'd'];
        reorder(&mut list, 0, 2);
        assert_eq!(list, ['b', 'c', 'a', 'd']);
        reorder(&mut list, 3, 0);
        assert_eq!(list, ['d', 'b', 'c', 'a']);
        reorder(&mut list, 1, 9);
        assert_eq!(list, ['d', 'b', 'c', 'a']);
    }

    #[test]
    fn requests() {
        assert_eq!(
            add_to_playlist_req("p", &ids(&["a", "b"]), "u"),
            Req {
                method: "POST",
                path: "/Playlists/p/Items".into(),
                query: vec![("ids", "a,b".into()), ("userId", "u".into())],
            }
        );
        assert_eq!(
            remove_from_playlist_req("p", &ids(&["e1"])),
            Req {
                method: "DELETE",
                path: "/Playlists/p/Items".into(),
                query: vec![("entryIds", "e1".into())],
            }
        );
        assert_eq!(move_in_playlist_req("p", "e1", 2).path, "/Playlists/p/Items/e1/Move/2");
        assert_eq!(create_collection_req("n", &ids(&["a"])).path, "/Collections");
        assert_eq!(add_to_collection_req("c", &ids(&["a", "b"])).query, vec![("ids", "a,b".to_string())]);
        assert_eq!(remove_from_collection_req("c", &ids(&["a"])).method, "DELETE");
    }

    #[test]
    fn bodies() {
        let body = create_playlist_body("n", &ids(&["a"]), "u");
        assert_eq!(body["Name"], "n");
        assert_eq!(body["Ids"][0], "a");
        assert_eq!(body["UserId"], "u");
        assert_eq!(body["MediaType"], "Video");
        assert_eq!(rename_playlist_body("x"), json!({ "Name": "x" }));
    }

    #[test]
    fn entries_decode() {
        let text = r#"{"Items":[{"Id":"i1","Name":"A","Type":"Movie","PlaylistItemId":"e1"},
                       {"Id":"i1","Name":"A","Type":"Movie","PlaylistItemId":"e2"}],"TotalRecordCount":2}"#;
        let entries: Entries = serde_json::from_str(text).unwrap();
        assert_eq!(entries.items.len(), 2);
        assert_eq!(entries.items[1].entry_id, "e2");
        assert_eq!(entries.items[0].item.name, "A");
    }

    #[test]
    fn kinds_for_lists() {
        assert!(can_add_to_playlist("Episode"));
        assert!(!can_add_to_playlist("BoxSet"));
        assert!(can_add_to_collection("Movie"));
        assert!(!can_add_to_collection("Episode"));
    }
}
