//! The Browser's organisation (project organisation brief, step 2):
//! nested groups (folders), each object's order, hidden and locked state.
//! Document state like everything else in this crate: immutable values,
//! changed only through `with_*` methods that return a new value, so it is
//! undoable, saved in 3MF metadata and replays exactly like any other
//! document change.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ObjectId;

/// A group's id. Stable for the session, never reused, like
/// [`ObjectId`] (its own counter, so a group and an object are never
/// confused even though both are small integers).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
pub struct GroupId(pub u64);

/// One entry of a group's (or the root's) order: an object or a nested
/// group, in Browser row order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Entry {
    Object(ObjectId),
    Group(GroupId),
}

/// A folder in the Browser: a name and, recursively, its own ordered
/// members (objects and nested groups). Its parent is implicit in
/// whichever order (the root's, or another group's) lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub id: GroupId,
    pub name: String,
    pub order: Vec<Entry>,
}

/// Document-level Browser state (project organisation brief, step 2):
/// nested groups, every object's order (inside its group, or the root),
/// and each object's hidden/locked flags. Cloning is cheap (small maps),
/// matching [`crate::Document`]'s own immutable-value style.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Organisation {
    groups: Vec<Group>,
    /// Root-level order: objects and groups with no parent group.
    root: Vec<Entry>,
    hidden: BTreeMap<ObjectId, bool>,
    locked: BTreeMap<ObjectId, bool>,
    next_group: u64,
}

/// Why a requested organisation change was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrganisationError {
    UnknownGroup,
    UnknownEntry,
    /// Moving a group into itself or one of its own descendants.
    Cycle,
}

impl Organisation {
    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    pub fn group(&self, id: GroupId) -> Option<&Group> {
        self.groups.iter().find(|g| g.id == id)
    }

    /// The root's order (objects and groups with no parent).
    pub fn root_order(&self) -> &[Entry] {
        &self.root
    }

    pub fn is_hidden(&self, id: ObjectId) -> bool {
        self.hidden.get(&id).copied().unwrap_or(false)
    }

    pub fn is_locked(&self, id: ObjectId) -> bool {
        self.locked.get(&id).copied().unwrap_or(false)
    }

    /// Where `entry` sits: `None` for the root, else the group containing
    /// it. `None` too if the entry isn't listed anywhere (an object new
    /// to the document: it belongs at the root until placed).
    pub fn parent_of(&self, entry: Entry) -> Option<GroupId> {
        if self.root.contains(&entry) {
            return None;
        }
        self.groups
            .iter()
            .find(|g| g.order.contains(&entry))
            .map(|g| g.id)
    }

    /// Every object a group contains, recursively (its own objects, and
    /// every object of every nested group): selecting a group selects
    /// all its members.
    pub fn members(&self, id: GroupId) -> Vec<ObjectId> {
        let mut out = Vec::new();
        self.collect_members(id, &mut out);
        out
    }

    fn collect_members(&self, id: GroupId, out: &mut Vec<ObjectId>) {
        let Some(g) = self.group(id) else { return };
        for e in &g.order {
            match e {
                Entry::Object(o) => out.push(*o),
                Entry::Group(child) => self.collect_members(*child, out),
            }
        }
    }

    /// A new group named `name` at the root, containing `entries` (moved
    /// out of wherever they were), inserted at the root in place of the
    /// first entry moved (Cmd+G on a selection, D-ORG-1): undoable like
    /// any other organisation change. Entries not currently anywhere (a
    /// brand new object) are simply added to the group.
    pub fn with_new_group(&self, name: String, entries: &[Entry]) -> (Organisation, GroupId) {
        let mut d = self.clone();
        let id = GroupId(d.next_group);
        d.next_group += 1;
        // Where the new group goes: the root position of the first moved
        // entry that was at the root; otherwise the end of the root.
        let root_insert_at = entries
            .iter()
            .find_map(|e| d.root.iter().position(|r| r == e));
        for e in entries {
            d.remove_entry(*e);
        }
        d.groups.push(Group {
            id,
            name,
            order: entries.to_vec(),
        });
        match root_insert_at {
            Some(at) => d.root.insert(at.min(d.root.len()), Entry::Group(id)),
            None => d.root.push(Entry::Group(id)),
        }
        (d, id)
    }

    /// Removes `entry` from wherever it currently sits (the root or any
    /// group's order), leaving everything else in place. A no-op if it
    /// isn't listed anywhere.
    fn remove_entry(&mut self, entry: Entry) {
        self.root.retain(|e| *e != entry);
        for g in &mut self.groups {
            g.order.retain(|e| *e != entry);
        }
    }

    /// True if `ancestor` is `descendant` or contains it, directly or
    /// through nested groups (cycle check for [`Self::with_moved`]).
    fn contains_group(&self, ancestor: GroupId, descendant: GroupId) -> bool {
        if ancestor == descendant {
            return true;
        }
        self.group(ancestor).is_some_and(|g| {
            g.order.iter().any(|e| match e {
                Entry::Group(child) => self.contains_group(*child, descendant),
                Entry::Object(_) => false,
            })
        })
    }

    /// Moves `entry` to sit at `index` of `into`'s order (`None`: the
    /// root), for drag-to-reorder and drag-between-groups. Refused if
    /// `into` doesn't exist, `entry` would move into itself, or (for a
    /// group entry) into one of its own descendants (a cycle).
    pub fn with_moved(
        &self,
        entry: Entry,
        into: Option<GroupId>,
        index: usize,
    ) -> Result<Organisation, OrganisationError> {
        if let Some(target) = into {
            if self.group(target).is_none() {
                return Err(OrganisationError::UnknownGroup);
            }
            if let Entry::Group(moved) = entry
                && self.contains_group(moved, target)
            {
                return Err(OrganisationError::Cycle);
            }
        }
        let mut d = self.clone();
        d.remove_entry(entry);
        let list = match into {
            None => &mut d.root,
            Some(g) => {
                &mut d
                    .groups
                    .iter_mut()
                    .find(|group| group.id == g)
                    .ok_or(OrganisationError::UnknownGroup)?
                    .order
            }
        };
        list.insert(index.min(list.len()), entry);
        Ok(d)
    }

    /// Renames group `id`. `None` if it doesn't exist.
    pub fn with_group_renamed(&self, id: GroupId, name: String) -> Option<Organisation> {
        let mut d = self.clone();
        d.groups.iter_mut().find(|g| g.id == id)?.name = name;
        Some(d)
    }

    /// Sets `id`'s hidden flag (Document state: undoable, 3MF metadata).
    pub fn with_hidden(&self, id: ObjectId, hidden: bool) -> Organisation {
        let mut d = self.clone();
        if hidden {
            d.hidden.insert(id, true);
        } else {
            d.hidden.remove(&id);
        }
        d
    }

    /// Sets `id`'s locked flag.
    pub fn with_locked(&self, id: ObjectId, locked: bool) -> Organisation {
        let mut d = self.clone();
        if locked {
            d.locked.insert(id, true);
        } else {
            d.locked.remove(&id);
        }
        d
    }

    /// Makes sure `id` is listed somewhere (appended to the root if it
    /// isn't): called after a command creates an object, so every object
    /// always has exactly one place in the tree.
    pub fn with_object_tracked(&self, id: ObjectId) -> Organisation {
        let entry = Entry::Object(id);
        if self.parent_of(entry).is_some() || self.root.contains(&entry) {
            return self.clone();
        }
        let mut d = self.clone();
        d.root.push(entry);
        d
    }

    /// Drops every trace of `id` (the root's order, every group's order,
    /// and its hidden/locked flags): called when a command removes an
    /// object. Undo of the remove restores the document to the previous
    /// revision, organisation included, since this whole value is part
    /// of it.
    pub fn without_object(&self, id: ObjectId) -> Organisation {
        let mut d = self.clone();
        d.remove_entry(Entry::Object(id));
        d.hidden.remove(&id);
        d.locked.remove(&id);
        d
    }

    /// Serialised as JSON for 3MF's `meshfixer:browser` model metadata
    /// (MF-DOC-004 extended to document-level organisation).
    pub fn to_json(&self) -> String {
        // allow-swallow: this type serializes to a JSON value with no error case
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Reads [`Self::to_json`] back; `None` for malformed or absent text
    /// (a 3MF with no Browser metadata, or from before this existed),
    /// which the caller treats as the default (everything at the root,
    /// nothing hidden or locked).
    pub fn from_json(s: &str) -> Option<Organisation> {
        serde_json::from_str(s).ok()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests may panic on failure

    use super::*;

    #[test]
    fn grouping_moves_entries_out_of_the_root_into_a_new_group() {
        let org = Organisation::default();
        let (org, _) = {
            let mut o = org;
            o.root = vec![Entry::Object(ObjectId(1)), Entry::Object(ObjectId(2))];
            o.with_new_group(
                "Assembly".into(),
                &[Entry::Object(ObjectId(1)), Entry::Object(ObjectId(2))],
            )
        };
        assert_eq!(org.root.len(), 1);
        let Entry::Group(gid) = org.root[0] else {
            panic!("expected a group at the root")
        };
        let g = org.group(gid).unwrap();
        assert_eq!(g.name, "Assembly");
        assert_eq!(
            g.order,
            vec![Entry::Object(ObjectId(1)), Entry::Object(ObjectId(2))]
        );
        assert_eq!(org.members(gid), vec![ObjectId(1), ObjectId(2)]);
    }

    #[test]
    fn nested_groups_report_every_descendant_as_a_member() {
        let org = Organisation {
            root: vec![Entry::Object(ObjectId(1))],
            ..Default::default()
        };
        let (org, outer) = org.with_new_group("Outer".into(), &[Entry::Object(ObjectId(1))]);
        let (org, inner) = org.with_new_group("Inner".into(), &[]);
        let org = org.with_moved(Entry::Group(inner), Some(outer), 0).unwrap();
        let org = org.with_moved(Entry::Object(ObjectId(99)), Some(inner), 0);
        // `with_moved` on an object not yet tracked still places it (it's
        // not required to exist as a real document object at this layer).
        let org = org.unwrap();
        // Outer directly holds object 1 and the nested Inner group, whose
        // own member (99) counts too: members() is recursive.
        assert_eq!(org.members(outer), vec![ObjectId(99), ObjectId(1)]);
        assert_eq!(org.members(inner), vec![ObjectId(99)]);
    }

    #[test]
    fn moving_a_group_into_its_own_descendant_is_a_cycle() {
        let org = Organisation::default();
        let (org, outer) = org.with_new_group("Outer".into(), &[]);
        let (org, inner) = org.with_new_group("Inner".into(), &[]);
        let org = org.with_moved(Entry::Group(inner), Some(outer), 0).unwrap();
        assert_eq!(
            org.with_moved(Entry::Group(outer), Some(inner), 0),
            Err(OrganisationError::Cycle)
        );
    }

    #[test]
    fn hidden_and_locked_default_false_and_round_trip_through_json() {
        let org = Organisation::default()
            .with_hidden(ObjectId(1), true)
            .with_locked(ObjectId(1), true);
        assert!(org.is_hidden(ObjectId(1)));
        assert!(org.is_locked(ObjectId(1)));
        assert!(!org.is_hidden(ObjectId(2)));
        let json = org.to_json();
        let back = Organisation::from_json(&json).unwrap();
        assert_eq!(back, org);
    }

    #[test]
    fn without_object_clears_its_order_entry_and_flags() {
        let org = Organisation {
            root: vec![Entry::Object(ObjectId(1)), Entry::Object(ObjectId(2))],
            ..Default::default()
        };
        let org = org.with_hidden(ObjectId(1), true).without_object(ObjectId(1));
        assert_eq!(org.root, vec![Entry::Object(ObjectId(2))]);
        assert!(!org.is_hidden(ObjectId(1)));
    }

    #[test]
    fn a_new_object_is_tracked_at_the_root_once() {
        let org = Organisation::default();
        let org = org.with_object_tracked(ObjectId(1));
        let org = org.with_object_tracked(ObjectId(1));
        assert_eq!(org.root, vec![Entry::Object(ObjectId(1))]);
    }
}
