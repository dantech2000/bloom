// SPDX-License-Identifier: AGPL-3.0-or-later
//! A server object in an editor, and the save of one. The server takes an
//! object whole, so a save sends the object the server has now with the
//! changes of the user in it, and nothing else: what another client
//! changed meanwhile stays. The settings of the user and the editors of
//! the dashboard save this way. The objects are `serde_json::Value` on
//! purpose: a typed struct would drop the fields the app does not know.

/// A server object in an editor: as the server gave it, and the copy the
/// user edits. Nothing goes to the server before a save, and the page
/// shows the Save bar while the two differ.
#[derive(Clone, Default)]
pub struct Draft {
    pub original: serde_json::Value,
    pub edited: serde_json::Value,
    /// A save is on its way.
    pub saving: bool,
}

impl Draft {
    pub fn new(value: serde_json::Value) -> Self {
        Self { edited: value.clone(), original: value, saving: false }
    }

    pub fn dirty(&self) -> bool {
        self.original != self.edited
    }

    pub fn discard(&mut self) {
        self.edited = self.original.clone();
    }

    /// The object to send: the one the server has now, with the edits in
    /// it (see [`with_changes`]).
    pub fn body(&self, fresh: &serde_json::Value) -> serde_json::Value {
        with_changes(fresh, &self.original, &self.edited)
    }

    /// The server took a save, and holds `server` now. What the user
    /// changed since the save went out (`sent` is the copy it was made
    /// from) stays an edit, on top of what the server holds.
    pub fn saved(&mut self, server: serde_json::Value, sent: &serde_json::Value) {
        self.edited = with_changes(&server, sent, &self.edited);
        self.original = server;
        self.saving = false;
    }
}

/// The object to send for a save: the object of the server now (`fresh`),
/// with the places the user changed (where `edited` differs from
/// `original`) taken from `edited`, and only those. What another client
/// or a save of another page changed since `original` was read stays. An
/// object inside is merged place by place; a list or a value goes whole.
pub fn with_changes(fresh: &serde_json::Value, original: &serde_json::Value, edited: &serde_json::Value) -> serde_json::Value {
    changed(Some(fresh), Some(original), Some(edited)).unwrap_or(serde_json::Value::Null)
}

/// One place of [`with_changes`]; `None` is a key that is not there, which
/// is not the same as a key with the value null.
fn changed(
    fresh: Option<&serde_json::Value>,
    original: Option<&serde_json::Value>,
    edited: Option<&serde_json::Value>,
) -> Option<serde_json::Value> {
    use serde_json::{Map, Value};
    if original == edited {
        return fresh.cloned();
    }
    let edited = edited?;
    let (Some(original_object), Some(edited_object)) = (original.and_then(Value::as_object), edited.as_object())
    else {
        return Some(edited.clone());
    };
    // The object is gone on the server, or is no object there any more:
    // the changes of the user go into an empty one, and the places the
    // user left alone do not come back from the old read.
    let empty = Map::new();
    let fresh_object = fresh.and_then(Value::as_object).unwrap_or(&empty);
    let mut merged = fresh_object.clone();
    for key in original_object.keys().chain(edited_object.keys().filter(|key| !original_object.contains_key(*key))) {
        let (was, now) = (original_object.get(key), edited_object.get(key));
        if was == now {
            continue;
        }
        match changed(fresh_object.get(key), was, now) {
            Some(value) => merged.insert(key.clone(), value),
            None => merged.remove(key),
        };
    }
    Some(Value::Object(merged))
}

#[cfg(test)]
mod tests {
    use super::with_changes;
    use serde_json::json;

    /// A save sends the change of the user and nothing else: a value that
    /// changed on the server since the page was read is not put back.
    #[test]
    fn a_save_keeps_what_changed_on_the_server_meanwhile() {
        let original = json!({ "ServerName": "old", "MinResumePct": 5, "Paths": { "Cache": "/a", "Temp": "/t" }, "Tags": ["x"] });
        let mut edited = original.clone();
        edited["ServerName"] = json!("new");
        edited["Paths"]["Cache"] = json!("/b");
        // Another page saved `MinResumePct` and `Paths.Temp`, and the
        // server has a key this app never saw.
        let fresh = json!({ "ServerName": "old", "MinResumePct": 10, "Paths": { "Cache": "/a", "Temp": "/t2" }, "Tags": ["x"], "New": true });
        assert_eq!(
            with_changes(&fresh, &original, &edited),
            json!({ "ServerName": "new", "MinResumePct": 10, "Paths": { "Cache": "/b", "Temp": "/t2" }, "Tags": ["x"], "New": true })
        );
    }

    #[test]
    fn no_change_sends_the_object_of_the_server_and_a_list_goes_whole() {
        let original = json!({ "A": 1, "List": [1, 2], "Gone": true });
        let fresh = json!({ "A": 2, "List": [9], "Gone": true });
        assert_eq!(with_changes(&fresh, &original, &original), fresh);
        let mut edited = original.clone();
        edited["List"] = json!([1, 2, 3]);
        edited.as_object_mut().unwrap().remove("Gone");
        assert_eq!(with_changes(&fresh, &original, &edited), json!({ "A": 2, "List": [1, 2, 3] }));
    }

    /// A key the user added with the value null is sent, though the server
    /// has another value there by now: null is not "not there".
    #[test]
    fn a_null_the_user_set_is_a_change() {
        assert_eq!(with_changes(&json!({ "A": 42 }), &json!({}), &json!({ "A": null })), json!({ "A": null }));
        assert_eq!(with_changes(&json!({ "A": 42 }), &json!({ "A": null }), &json!({})), json!({}));
    }

    /// The server dropped an object, or put a value in its place: the
    /// change of the user goes in, and what the user left alone in the old
    /// read does not come back.
    #[test]
    fn an_object_gone_on_the_server_takes_the_change_alone() {
        let original = json!({ "P": { "A": 1, "B": 2 } });
        let edited = json!({ "P": { "A": 3, "B": 2 } });
        assert_eq!(with_changes(&json!({}), &original, &edited), json!({ "P": { "A": 3 } }));
        assert_eq!(with_changes(&json!({ "P": 7 }), &original, &edited), json!({ "P": { "A": 3 } }));
    }
}
