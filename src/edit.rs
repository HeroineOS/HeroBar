//! Changes the bar makes to its own config (pinning apps from the
//! taskbar's menu), with toml_edit so comments and layout are kept. The
//! bar then picks the change up like any other edit.

use std::path::Path;

use toml_edit::{value, Array, DocumentMut, InlineTable, Item, Table, Value};

use crate::config::{Folder, Pinned};

/// A pinned entry: indexes down the folders, then within the last one.
pub type PinPath = Vec<usize>;

fn to_value(p: &Pinned) -> Value {
    match p {
        Pinned::App(id) => Value::from(id.as_str()),
        Pinned::Folder(f) => {
            let mut t = InlineTable::new();
            t.insert("folder", Value::from(f.folder.as_str()));
            if let Some(i) = &f.icon {
                t.insert("icon", Value::from(i.as_str()));
            }
            t.insert("apps", Value::Array(to_array(&f.apps)));
            Value::InlineTable(t)
        }
    }
}

fn to_array(list: &[Pinned]) -> Array {
    let mut a = Array::new();
    for p in list {
        a.push(to_value(p));
    }
    a
}

/// Edits the `pinned` list of taskbar module `module` in the config at
/// `path` (written from the built-in default first if there's none).
pub fn pinned(path: &Path, module: &str, f: impl FnOnce(&mut Vec<Pinned>)) -> Result<(), String> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|_| crate::config::DEFAULT.to_owned());
    let mut doc: DocumentMut = text.parse().map_err(|e| format!("{}: {e}", path.display()))?;
    let current = crate::config::parse(&text)?.modules.get(module).and_then(|m| m.pinned.clone()).unwrap_or_default();
    let mut list = current;
    f(&mut list);
    if !doc.contains_table("modules") {
        doc["modules"] = Item::Table(Table::new());
    }
    let modules = doc["modules"].as_table_mut().ok_or("[modules] isn't a table")?;
    modules.set_implicit(true);
    if !modules.contains_key(module) {
        modules[module] = Item::Table(Table::new());
    }
    let t = modules[module].as_table_mut().ok_or("module isn't a table")?;
    // Keep the comment after the key, if any.
    let decor = t.get("pinned").and_then(|i| i.as_value()).map(|v| v.decor().clone());
    t["pinned"] = value(to_array(&list));
    if let (Some(d), Some(v)) = (decor, t["pinned"].as_value_mut()) {
        *v.decor_mut() = d;
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// The list holding `path`'s entry, and its index there.
fn parent_mut<'a>(list: &'a mut Vec<Pinned>, path: &[usize]) -> Option<(&'a mut Vec<Pinned>, usize)> {
    let (&last, folders) = path.split_last()?;
    let mut cur = list;
    for &i in folders {
        cur = match cur.get_mut(i)? {
            Pinned::Folder(f) => &mut f.apps,
            Pinned::App(_) => return None,
        };
    }
    (last < cur.len()).then_some((cur, last))
}

fn folder_mut<'a>(list: &'a mut Vec<Pinned>, path: &[usize]) -> Option<&'a mut Folder> {
    let (parent, i) = parent_mut(list, path)?;
    match &mut parent[i] {
        Pinned::Folder(f) => Some(f),
        Pinned::App(_) => None,
    }
}

/// Where app `id` is pinned, if it is.
pub fn find(list: &[Pinned], id: &str) -> Option<PinPath> {
    for (i, p) in list.iter().enumerate() {
        match p {
            Pinned::App(a) if a == id => return Some(vec![i]),
            Pinned::Folder(f) => {
                if let Some(mut sub) = find(&f.apps, id) {
                    sub.insert(0, i);
                    return Some(sub);
                }
            }
            _ => {}
        }
    }
    None
}

/// Removes the entry at `path`.
pub fn remove(list: &mut Vec<Pinned>, path: &[usize]) -> Option<Pinned> {
    let (parent, i) = parent_mut(list, path)?;
    Some(parent.remove(i))
}

/// `target` after removing the entry at `removed` (later siblings move up).
fn shifted(mut target: PinPath, removed: &[usize]) -> PinPath {
    let (&r, prefix) = removed.split_last().expect("non-empty");
    if target.len() > prefix.len() && target[..prefix.len()] == *prefix && target[prefix.len()] > r {
        target[prefix.len()] -= 1;
    }
    target
}

/// Pins app `id` at the end.
pub fn pin(list: &mut Vec<Pinned>, id: &str) {
    if find(list, id).is_none() {
        list.push(Pinned::App(id.to_owned()));
    }
}

/// Moves app `id` (pinned or not) into the folder at `folder`.
pub fn move_to(list: &mut Vec<Pinned>, id: &str, folder: PinPath) {
    let mut folder = folder;
    if let Some(at) = find(list, id) {
        remove(list, &at);
        folder = shifted(folder, &at);
    }
    if let Some(f) = folder_mut(list, &folder) {
        f.apps.push(Pinned::App(id.to_owned()));
    }
}

/// Puts app `id` in a new folder where it was (or at the end).
pub fn new_folder(list: &mut Vec<Pinned>, id: &str, name: &str) {
    let folder = Pinned::Folder(Folder { folder: name.to_owned(), icon: None, apps: vec![Pinned::App(id.to_owned())] });
    match find(list, id) {
        Some(at) => {
            if let Some((parent, i)) = parent_mut(list, &at) {
                parent[i] = folder;
            }
        }
        None => list.push(folder),
    }
}

/// Moves the entry at `path` out of its folder, right after the folder.
pub fn move_out(list: &mut Vec<Pinned>, path: &[usize]) {
    if path.len() < 2 {
        return;
    }
    let Some(entry) = remove(list, path) else { return };
    let folder = &path[..path.len() - 1];
    if let Some((parent, i)) = parent_mut(list, folder) {
        parent.insert(i + 1, entry);
    }
}

/// Removes the folder at `path`, keeping what was in it in its place.
pub fn dissolve(list: &mut Vec<Pinned>, path: &[usize]) {
    let Some((parent, i)) = parent_mut(list, path) else { return };
    if let Pinned::Folder(f) = parent.remove(i) {
        for (k, p) in f.apps.into_iter().enumerate() {
            parent.insert(i + k, p);
        }
    }
}

/// Every folder: (path, "Games / Emulators").
pub fn folders(list: &[Pinned]) -> Vec<(PinPath, String)> {
    let mut out = Vec::new();
    fn walk(list: &[Pinned], path: &mut PinPath, name: &str, out: &mut Vec<(PinPath, String)>) {
        for (i, p) in list.iter().enumerate() {
            if let Pinned::Folder(f) = p {
                path.push(i);
                let full = if name.is_empty() { f.folder.clone() } else { format!("{name} / {}", f.folder) };
                out.push((path.clone(), full.clone()));
                walk(&f.apps, path, &full, out);
                path.pop();
            }
        }
    }
    walk(list, &mut Vec::new(), "", &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(s: &str) -> Pinned {
        Pinned::App(s.into())
    }
    fn folder(n: &str, apps: Vec<Pinned>) -> Pinned {
        Pinned::Folder(Folder { folder: n.into(), icon: None, apps })
    }

    #[test]
    fn pin_ops() {
        let mut l = vec![app("foot"), folder("Games", vec![app("steam"), folder("Emu", vec![])]), app("mpv")];
        assert_eq!(find(&l, "steam"), Some(vec![1, 0]));
        // mpv into Games/Emu.
        move_to(&mut l, "mpv", vec![1, 1]);
        assert_eq!(find(&l, "mpv"), Some(vec![1, 1, 0]));
        // foot (before Games) into Games: the folder's path shifts.
        move_to(&mut l, "foot", vec![1]);
        assert_eq!(find(&l, "foot"), Some(vec![0, 2]));
        move_out(&mut l, &[0, 2]);
        assert_eq!(find(&l, "foot"), Some(vec![1]));
        new_folder(&mut l, "foot", "Tools");
        assert_eq!(find(&l, "foot"), Some(vec![1, 0]));
        let names: Vec<String> = folders(&l).into_iter().map(|(_, n)| n).collect();
        assert_eq!(names, ["Games", "Games / Emu", "Tools"]);
        dissolve(&mut l, &[0]);
        assert_eq!(find(&l, "steam"), Some(vec![0]));
        assert_eq!(find(&l, "mpv"), Some(vec![1, 0]));
        remove(&mut l, &[1, 0]);
        assert!(find(&l, "mpv").is_none());
        pin(&mut l, "mpv");
        assert_eq!(find(&l, "mpv"), Some(vec![l.len() - 1]));
    }

    #[test]
    fn edits_keep_comments() {
        let dir = std::env::temp_dir().join(format!("herobar-edit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("bar.toml");
        std::fs::write(&p, "# mine\n[bar]\nmodules-left = [\"taskbar\"]\n\n[modules.taskbar]\npinned = [\"foot\"] # my apps\n").unwrap();
        pinned(&p, "taskbar", |l| new_folder(l, "foot", "Tools")).unwrap();
        let t = std::fs::read_to_string(&p).unwrap();
        assert!(t.contains("# mine") && t.contains("# my apps"), "{t}");
        assert!(t.contains(r#"{ folder = "Tools", apps = ["foot"] }"#), "{t}");
        crate::config::parse(&t).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
