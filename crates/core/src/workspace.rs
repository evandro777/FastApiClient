//! The open projects and the current selection: tree rows, create, rename, delete. No I/O and no UI, so it is all
//! unit-tested; the app saves the project an operation reports.
use crate::model::{Environment, Folder, Project, Request};

/// What is selected: a project, a folder of it, or a request (in a folder, or directly under the project).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sel {
    pub project: usize,
    pub folder: Option<usize>,
    pub request: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Project,
    Folder,
    Request,
}

impl Sel {
    pub fn kind(self) -> Kind {
        match (self.folder, self.request) {
            (_, Some(_)) => Kind::Request,
            (Some(_), None) => Kind::Folder,
            (None, None) => Kind::Project,
        }
    }
}

pub enum Node<'a> {
    Project(&'a mut Project),
    Folder(&'a mut Folder),
    Request(&'a mut Request),
}

pub enum Deleted {
    /// The file of this project id must be removed.
    Project(String),
    /// A folder or request went away; the project at this index must be saved.
    Part(usize),
}

pub struct TreeItem {
    pub sel: Sel,
    pub kind: Kind,
    pub depth: u8,
    pub text: String,
    /// Only for requests.
    pub method: String,
    pub selected: bool,
}

/// Emits the folders whose parent is `parent` (`None`: the top level), each followed by its subfolders and then its
/// requests. A dangling `parent` index shows the folder at the top level instead of hiding it. Each folder has one
/// parent, so a walk from the top cannot loop; a folder caught in a hand-made cycle is simply not reached.
fn walk(
    p: &Project,
    parent: Option<usize>,
    depth: u8,
    row: &mut impl FnMut(Option<usize>, Option<usize>, Kind, u8, &str, &str),
) {
    let at_top = |f: &Folder| f.parent.is_none_or(|i| i >= p.folders.len());
    for (fi, f) in p.folders.iter().enumerate() {
        if (parent.is_none() && at_top(f)) || (parent.is_some() && f.parent == parent && parent != Some(fi)) {
            row(Some(fi), None, Kind::Folder, depth, &f.name, "");
            walk(p, Some(fi), depth.saturating_add(1), row);
            for (ri, r) in f.requests.iter().enumerate() {
                row(
                    Some(fi),
                    Some(ri),
                    Kind::Request,
                    depth.saturating_add(1),
                    &r.name,
                    &r.method,
                );
            }
        }
    }
}

#[derive(Default)]
pub struct Workspace {
    pub projects: Vec<Project>,
    pub sel: Option<Sel>,
}

impl Workspace {
    /// Starts with the first project selected, if there is one.
    pub fn new(projects: Vec<Project>) -> Self {
        let sel = (!projects.is_empty()).then_some(Sel {
            project: 0,
            folder: None,
            request: None,
        });
        Self { projects, sel }
    }

    fn valid(&self, s: Sel) -> bool {
        let Some(p) = self.projects.get(s.project) else {
            return false;
        };
        let requests = match s.folder {
            Some(f) => match p.folders.get(f) {
                Some(folder) => &folder.requests,
                None => return false,
            },
            None => &p.requests,
        };
        s.request.is_none_or(|r| r < requests.len())
    }

    /// Ignores a selection that points at nothing (a stale one restored from the settings, for example).
    pub fn select(&mut self, s: Sel) -> bool {
        let ok = self.valid(s);
        if ok {
            self.sel = Some(s);
        }
        ok
    }

    pub fn current(&mut self) -> Option<Node<'_>> {
        let s = self.sel?;
        let p = self.projects.get_mut(s.project)?;
        Some(match (s.folder, s.request) {
            (None, None) => Node::Project(p),
            (Some(f), None) => Node::Folder(p.folders.get_mut(f)?),
            (None, Some(r)) => Node::Request(p.requests.get_mut(r)?),
            (Some(f), Some(r)) => Node::Request(p.folders.get_mut(f)?.requests.get_mut(r)?),
        })
    }

    pub fn name(&self) -> Option<&str> {
        let s = self.sel?;
        let p = self.projects.get(s.project)?;
        Some(match (s.folder, s.request) {
            (None, None) => &p.name,
            (Some(f), None) => &p.folders.get(f)?.name,
            (None, Some(r)) => &p.requests.get(r)?.name,
            (Some(f), Some(r)) => &p.folders.get(f)?.requests.get(r)?.name,
        })
    }

    /// The selected request with its parents, as `http::prepare` wants them.
    pub fn request_parts(&self) -> Option<(&Project, Vec<&Folder>, &Request)> {
        let s = self.sel?;
        let p = self.projects.get(s.project)?;
        let folders: Vec<&Folder> = s.folder.map_or(vec![], |f| {
            p.folder_chain(f).into_iter().map(|i| &p.folders[i]).collect()
        });
        let requests = match s.folder {
            Some(f) => &p.folders.get(f)?.requests,
            None => &p.requests,
        };
        Some((p, folders, requests.get(s.request?)?))
    }

    /// Returns the index of the project to save. An empty name is ignored.
    pub fn rename(&mut self, name: &str) -> Option<usize> {
        let name = name.trim();
        let project = self.sel?.project;
        if name.is_empty() {
            return None;
        }
        match self.current()? {
            Node::Project(p) => p.name = name.into(),
            Node::Folder(f) => f.name = name.into(),
            Node::Request(r) => r.name = name.into(),
        }
        Some(project)
    }

    /// Renames the selected project, whatever part of it is selected. Returns the index of the project to save; an
    /// empty name is ignored.
    pub fn rename_project(&mut self, name: &str) -> Option<usize> {
        let name = name.trim();
        let project = self.sel?.project;
        if name.is_empty() {
            return None;
        }
        self.projects.get_mut(project)?.name = name.into();
        Some(project)
    }

    /// Adds and selects a project; returns its index.
    pub fn add_project(&mut self, name: &str) -> usize {
        self.projects.push(Project::new(name));
        let project = self.projects.len() - 1;
        self.sel = Some(Sel {
            project,
            folder: None,
            request: None,
        });
        project
    }

    /// Adds an imported project and selects it; returns its index.
    pub fn import_project(&mut self, project: Project) -> usize {
        self.projects.push(project);
        let project = self.projects.len() - 1;
        self.sel = Some(Sel {
            project,
            folder: None,
            request: None,
        });
        project
    }

    /// Adds and selects a folder: inside the selected folder (or the folder of the selected request), else at the top
    /// level of the selected project.
    pub fn add_folder(&mut self, name: &str) -> Option<usize> {
        let s = self.sel?;
        let project = s.project;
        let p = self.projects.get_mut(project)?;
        p.folders.push(Folder {
            name: name.into(),
            parent: s.folder.filter(|f| *f < p.folders.len()),
            ..Folder::default()
        });
        self.sel = Some(Sel {
            project,
            folder: Some(p.folders.len() - 1),
            request: None,
        });
        Some(project)
    }

    /// Adds and selects a request in the selected folder, or directly in the selected project.
    pub fn add_request(&mut self, name: &str) -> Option<usize> {
        let s = self.sel?;
        let p = self.projects.get_mut(s.project)?;
        let requests = match s.folder {
            Some(f) => &mut p.folders.get_mut(f)?.requests,
            None => &mut p.requests,
        };
        requests.push(Request {
            name: name.into(),
            ..Request::default()
        });
        self.sel = Some(Sel {
            request: Some(requests.len() - 1),
            ..s
        });
        Some(s.project)
    }

    /// Deletes the selection (a folder takes its requests with it) and selects the parent.
    pub fn delete(&mut self) -> Option<Deleted> {
        let s = self.sel.filter(|s| self.valid(*s))?;
        match s.kind() {
            Kind::Project => {
                let removed = self.projects.remove(s.project);
                self.sel = (!self.projects.is_empty()).then(|| Sel {
                    project: s.project.min(self.projects.len() - 1),
                    folder: None,
                    request: None,
                });
                Some(Deleted::Project(removed.id))
            }
            Kind::Folder => {
                let p = &mut self.projects[s.project];
                let target = s.folder?;
                let parent = p.folders[target].parent;
                // The folder and everything below it go; the survivors' `parent` indexes shift down to match.
                let gone: Vec<bool> = (0..p.folders.len())
                    .map(|i| p.folder_chain(i).contains(&target))
                    .collect();
                let shift = |i: usize| i - gone[..i].iter().filter(|g| **g).count();
                let mut index = 0;
                p.folders.retain(|_| {
                    index += 1;
                    !gone[index - 1]
                });
                for f in &mut p.folders {
                    f.parent = f.parent.filter(|i| *i < gone.len() && !gone[*i]).map(shift);
                }
                self.sel = Some(Sel {
                    folder: parent.filter(|i| *i < gone.len() && !gone[*i]).map(shift),
                    request: None,
                    ..s
                });
                Some(Deleted::Part(s.project))
            }
            Kind::Request => {
                let p = &mut self.projects[s.project];
                match s.folder {
                    Some(f) => p.folders[f].requests.remove(s.request?),
                    None => p.requests.remove(s.request?),
                };
                self.sel = Some(Sel { request: None, ..s });
                Some(Deleted::Part(s.project))
            }
        }
    }

    /// Environment names of the selected project.
    pub fn env_names(&self) -> Vec<String> {
        let project = self.sel.and_then(|s| self.projects.get(s.project));
        project.map_or(vec![], |p| p.environments.iter().map(|e| e.name.clone()).collect())
    }

    /// Variable names of the active environment, sorted, for autocomplete.
    pub fn var_names(&self) -> Vec<String> {
        let mut v = self
            .sel
            .and_then(|s| self.projects.get(s.project))
            .map_or(vec![], |p| crate::resolve::names(p.active_values()));
        v.sort();
        if let Some(p) = self.sel.and_then(|s| self.projects.get(s.project)) {
            let mut names: Vec<&str> = p
                .requests
                .iter()
                .chain(p.folders.iter().flat_map(|f| &f.requests))
                .map(|r| r.name.as_str())
                .filter(|n| !n.is_empty() && !n.contains('\''))
                .collect();
            names.sort_unstable();
            names.dedup();
            v.extend(names.iter().map(|n| format!("$response('{n}', '$.')")));
        }
        v.extend(crate::resolve::FUNCTIONS.map(String::from));
        v
    }

    pub fn active_env_index(&self) -> Option<usize> {
        let p = self.projects.get(self.sel?.project)?;
        p.environments.iter().position(|e| e.name == p.active_env)
    }

    /// Case-insensitive, because the active environment is stored by name.
    fn unique_env_name(&self, project: usize, wanted: &str, skip: Option<usize>) -> String {
        let taken = |n: &str| {
            let envs = &self.projects[project].environments;
            envs.iter()
                .enumerate()
                .any(|(i, e)| Some(i) != skip && e.name.eq_ignore_ascii_case(n))
        };
        if !taken(wanted) {
            return wanted.into();
        }
        let mut n = 2;
        loop {
            let candidate = format!("{wanted} {n}");
            if !taken(&candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    /// Adds an empty environment to the selected project and makes it the active one. Returns the project to save.
    pub fn add_env(&mut self, name: &str) -> Option<usize> {
        let name = name.trim();
        let project = self.sel?.project;
        self.projects.get(project)?;
        if name.is_empty() {
            return None;
        }
        let name = self.unique_env_name(project, name, None);
        let p = &mut self.projects[project];
        p.environments.push(Environment {
            name: name.clone(),
            values: serde_json::json!({}),
        });
        p.active_env = name;
        Some(project)
    }

    pub fn rename_env(&mut self, index: usize, name: &str) -> Option<usize> {
        let name = name.trim();
        let project = self.sel?.project;
        if name.is_empty() || index >= self.projects.get(project)?.environments.len() {
            return None;
        }
        let name = self.unique_env_name(project, name, Some(index));
        let p = &mut self.projects[project];
        let old = std::mem::replace(&mut p.environments[index].name, name.clone());
        if p.active_env == old {
            p.active_env = name;
        }
        Some(project)
    }

    pub fn delete_env(&mut self, index: usize) -> Option<usize> {
        let project = self.sel?.project;
        let p = self.projects.get_mut(project)?;
        if index >= p.environments.len() {
            return None;
        }
        let removed = p.environments.remove(index);
        if p.active_env == removed.name {
            p.active_env.clear();
        }
        Some(project)
    }

    /// `None` selects no environment. Returns the project to save, or `None` when nothing changed.
    pub fn set_active_env(&mut self, index: Option<usize>) -> Option<usize> {
        let project = self.sel?.project;
        let p = self.projects.get_mut(project)?;
        let name = index
            .and_then(|i| p.environments.get(i))
            .map_or("", |e| e.name.as_str());
        if p.active_env == name {
            return None;
        }
        p.active_env = name.into();
        Some(project)
    }

    pub fn env_text(&self, index: usize) -> Option<String> {
        Some(self.projects.get(self.sel?.project)?.environments.get(index)?.pretty())
    }

    /// Returns the project to save, or `None` when the values did not change.
    pub fn set_env_values(&mut self, index: usize, values: serde_json::Value) -> Option<usize> {
        let project = self.sel?.project;
        let env = self.projects.get_mut(project)?.environments.get_mut(index)?;
        if env.values == values {
            return None;
        }
        env.values = values;
        Some(project)
    }

    pub fn project_names(&self) -> Vec<String> {
        self.projects.iter().map(|p| p.name.clone()).collect()
    }

    /// Makes `index` the active project and selects the project itself.
    pub fn select_project(&mut self, index: usize) -> bool {
        self.select(Sel {
            project: index,
            folder: None,
            request: None,
        })
    }

    /// Flat rows for the sidebar, for the active project only: its folders (subfolders first, then the folder's
    /// requests), then its own requests. The project has no row: it is picked in the select box, and its shared
    /// headers and auth are opened with "Global config" (`select_project`).
    pub fn tree(&self) -> Vec<TreeItem> {
        let mut out = vec![];
        let active = self.sel.map(|s| s.project);
        for (pi, p) in self.projects.iter().enumerate().filter(|(pi, _)| Some(*pi) == active) {
            let mut row = |folder, request, kind, depth, text: &str, method: &str| {
                let sel = Sel {
                    project: pi,
                    folder,
                    request,
                };
                out.push(TreeItem {
                    sel,
                    kind,
                    depth,
                    text: text.into(),
                    method: method.into(),
                    selected: self.sel == Some(sel),
                });
            };
            walk(p, None, 0, &mut row);
            for (ri, r) in p.requests.iter().enumerate() {
                row(None, Some(ri), Kind::Request, 0, &r.name, &r.method);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(ws: &Workspace) -> Vec<(u8, String)> {
        ws.tree().into_iter().map(|t| (t.depth, t.text)).collect()
    }

    /// Project "P" > folder "F" > request "in-folder", plus request "root" directly under P.
    fn sample() -> Workspace {
        let mut ws = Workspace::default();
        ws.add_project("P");
        ws.add_folder("F");
        ws.add_request("in-folder");
        ws.select(Sel {
            project: 0,
            folder: None,
            request: None,
        });
        ws.add_request("root");
        ws
    }

    #[test]
    fn tree_lists_folders_before_root_requests() {
        let ws = sample();
        let expected = [(0, "F"), (1, "in-folder"), (0, "root")];
        assert_eq!(names(&ws), expected.map(|(d, t)| (d, t.to_string())));
        assert_eq!(ws.tree().iter().filter(|t| t.selected).count(), 1);
    }

    /// P > A > (B > "deep", "in-a"), C, plus a sibling folder D at the top. Folder indexes: A0 B1 C2 D3.
    fn nested() -> Workspace {
        let mut ws = Workspace::default();
        ws.add_project("P");
        ws.add_folder("A");
        ws.add_folder("B"); // A is selected, so B is inside A
        ws.add_request("deep");
        ws.select(Sel {
            project: 0,
            folder: Some(0),
            request: None,
        });
        ws.add_request("in-a");
        ws.select_project(0);
        ws.add_folder("C");
        ws.add_folder("D");
        ws.projects[0].folders[2].parent = Some(0);
        ws.projects[0].folders[3].parent = None;
        ws
    }

    #[test]
    fn nested_folders_show_subfolders_before_requests() {
        let ws = nested();
        let expected = [(0, "A"), (1, "B"), (2, "deep"), (1, "C"), (1, "in-a"), (0, "D")];
        assert_eq!(names(&ws), expected.map(|(d, t)| (d, t.to_string())));
        assert_eq!(ws.projects[0].folder_chain(1), [0, 1]);
    }

    #[test]
    fn a_new_folder_goes_inside_the_selected_folder() {
        let mut ws = nested();
        ws.select(Sel {
            project: 0,
            folder: Some(1),
            request: Some(0),
        });
        ws.add_folder("E");
        assert_eq!(
            ws.projects[0].folders[4].parent,
            Some(1),
            "next to a request: inside its folder"
        );
        ws.select_project(0);
        ws.add_folder("F");
        assert_eq!(
            ws.projects[0].folders[5].parent, None,
            "with the project selected: top level"
        );
    }

    #[test]
    fn deleting_a_folder_takes_its_subtree_and_remaps_parents() {
        let mut ws = nested();
        ws.select(Sel {
            project: 0,
            folder: Some(1),
            request: None,
        });
        ws.delete();
        assert_eq!(ws.sel.unwrap().folder, Some(0), "the parent folder is selected");
        ws.select(Sel {
            project: 0,
            folder: Some(0),
            request: None,
        });
        ws.delete();
        // A, its subfolders B and C and all their requests are gone; D shifts to index 0.
        assert_eq!(names(&ws), [(0, "D".to_string())]);
        assert_eq!(ws.projects[0].folders[0].parent, None);
    }

    #[test]
    fn a_dangling_or_cyclic_parent_cannot_hang_or_hide_the_tree() {
        let mut ws = nested();
        ws.projects[0].folders[3].parent = Some(99);
        ws.projects[0].folders[0].parent = Some(1);
        ws.projects[0].folders[1].parent = Some(0);
        assert!(
            names(&ws).iter().any(|(_, t)| t == "D"),
            "a dangling parent lists at the top"
        );
        assert!(ws.projects[0].folder_chain(0).len() <= 2, "a cycle ends the chain");
    }

    #[test]
    fn the_tree_only_shows_the_active_project() {
        let mut ws = sample();
        ws.add_project("Q");
        ws.add_request("only-in-q");
        assert_eq!(names(&ws), [(0, "only-in-q".to_string())]);
        assert_eq!(ws.project_names(), ["P", "Q"]);

        assert!(ws.select_project(0));
        assert_eq!(
            names(&ws).len(),
            3,
            "back on P: its folder and two requests, no project row"
        );
        assert!(
            ws.tree().iter().all(|t| !t.selected),
            "the project itself has no row to highlight"
        );
        assert_eq!(
            ws.sel.unwrap().kind(),
            Kind::Project,
            "switching opens the project itself"
        );
        assert!(!ws.select_project(5));
        assert_eq!(ws.sel.unwrap().project, 0, "an out-of-range index changes nothing");

        ws.sel = None;
        assert!(ws.tree().is_empty());
    }

    #[test]
    fn new_items_go_where_the_selection_is_and_get_selected() {
        let mut ws = sample();
        assert_eq!(ws.sel.unwrap().kind(), Kind::Request);
        assert_eq!(ws.projects[0].requests.len(), 1);
        // With a request selected, a new request lands next to it.
        ws.add_request("second");
        assert_eq!(ws.projects[0].requests[1].name, "second");
        // With a folder selected, it lands inside the folder.
        ws.select(Sel {
            project: 0,
            folder: Some(0),
            request: None,
        });
        ws.add_request("another");
        assert_eq!(ws.projects[0].folders[0].requests[1].name, "another");
        assert_eq!(ws.name(), Some("another"));
    }

    #[test]
    fn rename_project_works_from_any_selection() {
        let mut ws = sample(); // a request is selected
        assert_eq!(ws.rename_project("  Shop "), Some(0));
        assert_eq!(ws.projects[0].name, "Shop");
        assert_eq!(ws.name(), Some("root"), "the selected request keeps its name");
        assert_eq!(ws.rename_project("   "), None);
        assert_eq!(ws.projects[0].name, "Shop");
    }

    #[test]
    fn rename_trims_and_ignores_empty() {
        let mut ws = sample();
        assert_eq!(ws.rename("  renamed "), Some(0));
        assert_eq!(ws.name(), Some("renamed"));
        assert_eq!(ws.rename("   "), None);
        assert_eq!(ws.name(), Some("renamed"));
    }

    #[test]
    fn delete_selects_the_parent() {
        let mut ws = sample();
        assert!(matches!(ws.delete(), Some(Deleted::Part(0))));
        assert_eq!(ws.sel.unwrap().kind(), Kind::Project);
        ws.select(Sel {
            project: 0,
            folder: Some(0),
            request: None,
        });
        assert!(matches!(ws.delete(), Some(Deleted::Part(0))));
        assert!(ws.projects[0].folders.is_empty());
        let id = ws.projects[0].id.clone();
        assert!(matches!(ws.delete(), Some(Deleted::Project(i)) if i == id));
        assert!(ws.sel.is_none() && ws.tree().is_empty());
        assert!(ws.delete().is_none());
    }

    #[test]
    fn deleting_a_project_selects_a_neighbour() {
        let mut ws = Workspace::default();
        ws.add_project("a");
        ws.add_project("b");
        ws.select(Sel {
            project: 1,
            folder: None,
            request: None,
        });
        ws.delete();
        assert_eq!(ws.sel.map(|s| s.project), Some(0));
    }

    #[test]
    fn stale_selection_is_refused() {
        let mut ws = sample();
        let before = ws.sel;
        for bad in [
            Sel {
                project: 9,
                folder: None,
                request: None,
            },
            Sel {
                project: 0,
                folder: Some(5),
                request: None,
            },
            Sel {
                project: 0,
                folder: None,
                request: Some(7),
            },
        ] {
            assert!(!ws.select(bad));
        }
        assert_eq!(ws.sel, before);
    }

    #[test]
    fn environments_are_added_renamed_and_deleted_per_project() {
        let mut ws = sample();
        assert_eq!(ws.add_env("Local"), Some(0));
        // Names are unique ignoring case, since the active one is kept by name.
        assert_eq!(ws.add_env("local"), Some(0));
        assert_eq!(ws.env_names(), ["Local", "local 2"]);
        assert_eq!(ws.active_env_index(), Some(1));
        assert_eq!(ws.add_env("  "), None);

        assert_eq!(ws.rename_env(1, "Prod"), Some(0));
        assert_eq!(ws.projects[0].active_env, "Prod", "the active one follows its rename");
        assert_eq!(ws.rename_env(0, "prod"), Some(0));
        assert_eq!(ws.env_names(), ["prod 2", "Prod"]);

        assert_eq!(ws.set_active_env(Some(0)), Some(0));
        assert_eq!(ws.set_active_env(Some(0)), None, "no change, nothing to save");
        assert_eq!(ws.delete_env(0), Some(0));
        assert_eq!(ws.active_env_index(), None, "deleting the active one leaves none");
        assert_eq!(ws.delete_env(5), None);
    }

    #[test]
    fn environment_values_feed_the_request() {
        let mut ws = sample();
        ws.add_env("Local");
        assert_eq!(ws.set_env_values(0, serde_json::json!({"host": "h"})), Some(0));
        assert_eq!(ws.set_env_values(0, serde_json::json!({"host": "h"})), None);
        assert!(ws.env_text(0).unwrap().contains("\"host\": \"h\""));
        let (project, _, _) = ws.request_parts().unwrap();
        assert_eq!(project.active_values()["host"], "h");
    }

    #[test]
    fn request_parts_only_for_requests() {
        let mut ws = sample();
        let (p, folder, r) = ws.request_parts().unwrap();
        assert_eq!(
            (p.name.as_str(), folder.is_empty(), r.name.as_str()),
            ("P", true, "root")
        );
        ws.select(Sel {
            project: 0,
            folder: Some(0),
            request: None,
        });
        assert!(ws.request_parts().is_none());
    }
}
