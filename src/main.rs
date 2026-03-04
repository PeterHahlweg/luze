use std::{collections::HashSet, env, io::Write, path::{Path, PathBuf}, process};
use std::process::Command;
use luze::{ID, Note, NoteBox, MergeAction, merge_conflicts, merge_conflicts_rename_head};

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_help();
        process::exit(1);
    }

    match args[1].as_str() {
        "init"      => cmd_init(&args),
        "add"       => cmd_add(&args),
        "update"    => cmd_update(&args),
        "link"      => cmd_link(&args),
        "unlink"    => cmd_unlink(&args),
        "list"      => cmd_list(&args),
        "show"      => cmd_show(&args),
        "children"  => cmd_children(&args),
        "ancestors" => cmd_ancestors(&args),
        "backlinks" => cmd_backlinks(&args),
        "search"    => cmd_search(&args),
        "tree"      => cmd_tree(&args),
        "merge"     => cmd_merge(),
        "sync"      => cmd_sync(&args),
        "help" | "--help" | "-h" => print_help(),
        cmd => {
            eprintln!("error: unknown command '{}'", cmd);
            process::exit(1);
        }
    }
}

fn cmd_init(args: &[String]) {
    let global = args.iter().any(|a| a == "--global");
    let dir = if global {
        env::var("HOME").map(|h| PathBuf::from(h).join(".luze")).unwrap_or_else(|_| {
            eprintln!("error: could not determine home directory");
            process::exit(1);
        })
    } else if let Ok(p) = env::var("LUZE_PATH") {
        PathBuf::from(p)
    } else {
        PathBuf::from("./.luze")
    };
    let notes = NoteBox::create(dir.clone());
    save_notes(&notes);
    if !dir.join(".git").is_dir() && has_git() {
        let status = Command::new("git").args(["init"]).current_dir(&dir).status();
        match status {
            Ok(s) if s.success() => {
                eprint!("git remote url (enter to skip): ");
                std::io::stderr().flush().ok();
                let mut remote = String::new();
                if std::io::stdin().read_line(&mut remote).is_ok() {
                    let remote = remote.trim();
                    if !remote.is_empty() {
                        match git(&dir, &["remote", "add", "origin", remote]) {
                            Ok(_) => eprintln!("remote origin set to {}", remote),
                            Err(e) => eprintln!("warning: git remote add failed: {}", e),
                        }
                    }
                }
            }
            Ok(s) => eprintln!("warning: git init exited with {}", s),
            Err(e) => eprintln!("warning: git init failed: {}", e),
        }
    }
}

fn cmd_add(args: &[String]) {
    if args.len() < 4 {
        eprintln!("error: usage: luze add <id> <content>");
        eprintln!("hint:  a note must be atomic and dense (max 250 chars)");
        process::exit(1);
    }
    let id = ID::from(args[2].as_str());
    let content = args[3..].join(" ");
    validate_content(&content);
    let mut notes = load_notes();
    let parent = id.parent();
    if parent != id {
        match notes.find(&parent) {
            Ok(None) => {
                eprintln!("error: parent {} not found", parent);
                process::exit(1);
            }
            Err(e) => {
                eprintln!("error: {}", e);
                process::exit(1);
            }
            Ok(Some(_)) => {}
        }
    }
    if let Err(e) = notes.add(Note::new(id, parent, &content)) {
        eprintln!("error: {}", e);
        eprintln!("hint:  use 'luze update {} <content>' to create a new version", args[2]);
        process::exit(1);
    }
    save_notes(&notes);
    sync_hint();
}

fn cmd_update(args: &[String]) {
    if args.len() < 4 {
        eprintln!("error: usage: luze update <id> <content>");
        eprintln!("hint:  a note must be atomic and dense (max 250 chars)");
        process::exit(1);
    }
    let id = ID::from(args[2].as_str());
    let content = args[3..].join(" ");
    validate_content(&content);
    let mut notes = load_notes();
    match notes.update(&id, &content) {
        Ok(new_id) => {
            save_notes(&notes);
            println!("{} supersedes {}", new_id, id);
            sync_hint();
        }
        Err(e) => {
            eprintln!("error: {}", e);
            process::exit(1);
        }
    }
}

fn cmd_link(args: &[String]) {
    if args.len() < 4 {
        eprintln!("error: usage: luze link <from> <to>");
        process::exit(1);
    }
    let from = ID::from(args[2].as_str());
    let to   = ID::from(args[3].as_str());
    let mut notes = load_notes();
    match notes.find_mut(&from) {
        Ok(Some(note)) => note.add_link(to),
        Ok(None) => {
            eprintln!("error: note {} not found", from);
            process::exit(1);
        }
        Err(e) => {
            eprintln!("error: {}", e);
            process::exit(1);
        }
    }
    save_notes(&notes);
    sync_hint();
}

fn cmd_unlink(args: &[String]) {
    if args.len() < 4 {
        eprintln!("error: usage: luze unlink <from> <to>");
        process::exit(1);
    }
    let from = ID::from(args[2].as_str());
    let to   = ID::from(args[3].as_str());
    let mut notes = load_notes();
    match notes.find_mut(&from) {
        Ok(Some(note)) => {
            if !note.remove_link(&to) {
                eprintln!("error: no link from {} to {}", from, to);
                process::exit(1);
            }
            save_notes(&notes);
            sync_hint();
        }
        Ok(None) => {
            eprintln!("error: note {} not found", from);
            process::exit(1);
        }
        Err(e) => {
            eprintln!("error: {}", e);
            process::exit(1);
        }
    }
}

fn cmd_list(args: &[String]) {
    let show_all = args.iter().any(|a| a == "--all");
    let mut notes = load_notes();
    notes.load_all().unwrap_or_else(|e| { eprintln!("error: {}", e); process::exit(1) });
    let superseded: HashSet<&ID> = if show_all { HashSet::new() } else { notes.superseded_ids() };
    for note in notes.notes() {
        if !show_all && superseded.contains(note.id()) { continue; }
        println!("{:<6} {}", note.id(), headline(note.content()));
    }
}

fn cmd_show(args: &[String]) {
    if args.len() < 3 {
        eprintln!("error: usage: luze show <id>");
        process::exit(1);
    }
    let id = ID::from(args[2].as_str());
    let mut notes = load_notes();
    notes.load_all().unwrap_or_else(|e| { eprintln!("error: {}", e); process::exit(1) });
    match notes.find(&id) {
        Ok(Some(note)) => {
            let note = note.clone();
            println!("ID:      {}", note.id());
            println!("Created: {}", note.created_at().format("%Y-%m-%d %H:%M:%S UTC"));
            println!("Content: {}", note.content());
            if let Some(sup) = note.supersedes() {
                println!("Supersedes: {}", sup);
            }
            if let Some(by) = notes.superseded_by(note.id()) {
                println!("Superseded by: {}", by);
            }
            let links = note.links();
            if !links.is_empty() {
                let joined: Vec<String> = links.iter().map(|l| l.to_string()).collect();
                println!("Links:   {}", joined.join(", "));
            }
        }
        Ok(None) => {
            eprintln!("error: note {} not found", id);
            process::exit(1);
        }
        Err(e) => {
            eprintln!("error: {}", e);
            process::exit(1);
        }
    }
}

fn cmd_children(args: &[String]) {
    if args.len() < 3 {
        eprintln!("error: usage: luze children <id>");
        process::exit(1);
    }
    let id = ID::from(args[2].as_str());
    let mut notes = load_notes();
    let children = notes.children(&id)
        .unwrap_or_else(|e| { eprintln!("error: {}", e); process::exit(1) });
    for note in children {
        println!("{:<6} {}", note.id(), headline(note.content()));
    }
}

fn cmd_ancestors(args: &[String]) {
    if args.len() < 3 {
        eprintln!("error: usage: luze ancestors <id>");
        process::exit(1);
    }
    let id = ID::from(args[2].as_str());
    let mut notes = load_notes();
    let ancestors = notes.ancestors(&id)
        .unwrap_or_else(|e| { eprintln!("error: {}", e); process::exit(1) });
    for note in &ancestors {
        println!("{:<6} {}", note.id(), headline(note.content()));
    }
}

fn cmd_backlinks(args: &[String]) {
    if args.len() < 3 {
        eprintln!("error: usage: luze backlinks <id>");
        process::exit(1);
    }
    let id = ID::from(args[2].as_str());
    let mut notes = load_notes();
    let backlinks = notes.backlinks(&id)
        .unwrap_or_else(|e| { eprintln!("error: {}", e); process::exit(1) });
    for note in backlinks {
        println!("{:<6} {}", note.id(), headline(note.content()));
    }
}

fn cmd_search(args: &[String]) {
    if args.len() < 3 {
        eprintln!("error: usage: luze search <query>");
        process::exit(1);
    }
    let show_all = args.iter().any(|a| a == "--all");
    let query: String = args[2..].iter()
        .filter(|a| a.as_str() != "--all")
        .cloned().collect::<Vec<_>>().join(" ");
    let mut notes = load_notes();
    let results = if show_all {
        notes.search_all(&query)
    } else {
        notes.search(&query)
    }.unwrap_or_else(|e| { eprintln!("error: {}", e); process::exit(1) });
    for note in results {
        println!("{:<6} {}", note.id(), headline(note.content()));
    }
}

fn cmd_tree(args: &[String]) {
    let mut max_depth = usize::MAX;
    let mut root_id: Option<ID> = None;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "-d" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: -d requires a depth argument");
                    process::exit(1);
                }
                max_depth = args[i].parse().unwrap_or_else(|_| {
                    eprintln!("error: depth must be a non-negative integer");
                    process::exit(1)
                });
            }
            id_str => root_id = Some(ID::from(id_str)),
        }
        i += 1;
    }

    let mut notes = load_notes();
    notes.load_all().unwrap_or_else(|e| { eprintln!("error: {}", e); process::exit(1) });
    let all_notes = notes.notes();
    let superseded = notes.superseded_ids();

    if let Some(ref id) = root_id {
        if all_notes.iter().all(|n| n.id() != id) {
            eprintln!("error: note {} not found", id);
            process::exit(1);
        }
        print_tree(&all_notes, &superseded, id, 0, max_depth, "", true);
    } else {
        let roots: Vec<&Note> = all_notes.iter()
            .filter(|n| n.parent().map_or(false, |p| p == n.id()))
            .copied()
            .collect();
        let last = roots.len().saturating_sub(1);
        for (i, root) in roots.iter().enumerate() {
            print_tree(&all_notes, &superseded, root.id(), 0, max_depth, "", i == last);
        }
    }
}

fn cmd_merge() {
    match merge_conflicts(&notes_dir()) {
        Ok(reports) if reports.is_empty() => println!("no conflicts found"),
        Ok(reports) => {
            let mut conflicts: Vec<(&ID, &ID)> = Vec::new();
            for report in &reports {
                let name = if report.draw.to_string().is_empty() { "root".to_string() }
                           else { report.draw.to_string() };
                println!("draw {}:", name);
                for action in &report.actions {
                    match action {
                        MergeAction::Added(id) =>
                            println!("  added   {}", id),
                        MergeAction::LinksMerged(id) =>
                            println!("  links   {} (merged)", id),
                        MergeAction::Renamed { original, renamed_to } => {
                            println!("  renamed {} → {}", original, renamed_to);
                            conflicts.push((original, renamed_to));
                        }
                    }
                }
            }
            if !conflicts.is_empty() {
                println!();
                println!("semantic review required — both versions were kept but their");
                println!("meaning cannot be checked automatically:");
                let mut notes = load_notes();
                for (orig, renamed) in &conflicts {
                    for (label, id) in [("head ", orig), ("their", renamed)] {
                        print!("\n  [{}] {}", label, id);
                        if let Ok(Some(note)) = notes.find(id) {
                            println!(" — {}", note.content());
                        } else {
                            println!();
                        }
                    }
                }
                println!();
            }
        }
        Err(e) => { eprintln!("error: {}", e); process::exit(1); }
    }
}

fn print_tree(all: &[&Note], superseded: &HashSet<&ID>, id: &ID, depth: usize, max_depth: usize, prefix: &str, is_last: bool) {
    let note = all.iter().find(|n| n.id() == id);

    // Version marker: [v2] if this note supersedes another, [outdated] if it is itself superseded
    let marker = if superseded.contains(id) {
        " [outdated]"
    } else if note.map_or(false, |n| n.supersedes().is_some()) {
        " [v2]"  // could count generations, but v2 is clear enough for the tree
    } else {
        ""
    };

    let preview = headline(note.map_or("", |n| n.content()));
    let connector = if depth == 0 { "" } else if is_last { "└── " } else { "├── " };
    println!("{}{}{}{} {}", prefix, connector, id, marker, preview);

    if depth >= max_depth { return; }

    let child_prefix = if depth == 0 {
        prefix.to_string()
    } else if is_last {
        format!("{}    ", prefix)
    } else {
        format!("{}│   ", prefix)
    };

    let children: Vec<&Note> = all.iter()
        .filter(|n| n.id() != id && n.id().is_direct_child_of(id))
        .copied()
        .collect();
    let last = children.len().saturating_sub(1);
    for (i, child) in children.iter().enumerate() {
        print_tree(all, superseded, child.id(), depth + 1, max_depth, &child_prefix, i == last);
    }
}

fn has_git() -> bool {
    Command::new("git").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git").args(args).current_dir(dir).output()
        .map_err(|e| format!("failed to run git: {}", e))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(stderr)
    }
}

fn first_remote(dir: &Path) -> Option<String> {
    git(dir, &["remote"]).ok()
        .and_then(|s| s.lines().next().map(|l| l.to_string()))
        .filter(|s| !s.is_empty())
}

fn has_uncommitted(dir: &Path) -> bool {
    git(dir, &["status", "--porcelain"]).map(|s| !s.is_empty()).unwrap_or(false)
}

fn current_branch(dir: &Path) -> Option<String> {
    git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).ok().filter(|s| !s.is_empty() && s != "HEAD")
}

fn has_upstream(dir: &Path) -> bool {
    git(dir, &["rev-parse", "--abbrev-ref", "@{u}"]).is_ok()
}

fn unpushed_count(dir: &Path) -> usize {
    git(dir, &["rev-list", "--count", "@{u}..HEAD"])
        .ok().and_then(|s| s.parse().ok()).unwrap_or(0)
}

fn cmd_sync(args: &[String]) {
    let dir = notes_dir();
    if !dir.join(".git").is_dir() {
        eprintln!("error: {} is not a git repository", dir.display());
        eprintln!("hint:  run 'luze init' to create one, or 'git init' inside {}", dir.display());
        process::exit(1);
    }
    let remote = match first_remote(&dir) {
        Some(r) => r,
        None => {
            eprintln!("error: no git remote configured");
            eprintln!("hint:  run 'git -C {} remote add origin <url>'", dir.display());
            process::exit(1);
        }
    };

    // Optional commit message: luze sync -m "message"
    let mut message = String::from("luze sync");
    let mut i = 2;
    while i < args.len() {
        if args[i] == "-m" && i + 1 < args.len() {
            message = args[i + 1].clone();
            i += 2;
        } else {
            i += 1;
        }
    }

    // Step 1: commit local changes if any
    if has_uncommitted(&dir) {
        if let Err(e) = git(&dir, &["add", "-A"]) {
            eprintln!("error: git add failed: {}", e);
            process::exit(1);
        }
        if let Err(e) = git(&dir, &["commit", "-m", &message]) {
            eprintln!("error: git commit failed: {}", e);
            process::exit(1);
        }
    }

    let branch = current_branch(&dir).unwrap_or_else(|| "main".to_string());
    let tracking = has_upstream(&dir);

    // Step 2: pull (skip if no upstream yet — first push will set it)
    let pull_result = if tracking {
        git(&dir, &["pull"])
    } else {
        // Try fetching the remote branch; if it doesn't exist yet, skip pull
        match git(&dir, &["fetch", &remote, &branch]) {
            Ok(_) => git(&dir, &["merge", &format!("{}/{}", remote, branch)]),
            Err(_) => Ok(String::new()), // remote branch doesn't exist yet
        }
    };
    match pull_result {
        Ok(_) => {}
        Err(e) => {
            // Check if pull failed due to merge conflicts
            let status = git(&dir, &["status", "--porcelain"]);
            let has_conflicts = status.as_ref().map(|s| s.contains("UU")).unwrap_or(false);
            if has_conflicts {
                // Run luze merge to resolve draw conflicts (rename ours, keep upstream)
                match merge_conflicts_rename_head(&dir) {
                    Ok(reports) if reports.is_empty() => {
                        eprintln!("error: git pull failed with non-draw conflicts: {}", e);
                        process::exit(1);
                    }
                    Ok(reports) => {
                        for report in &reports {
                            for action in &report.actions {
                                if let MergeAction::Renamed { original, renamed_to } = action {
                                    eprintln!("renamed local {} → {} (upstream kept original ID)", original, renamed_to);
                                }
                            }
                        }
                        if let Err(e) = git(&dir, &["add", "-A"]) {
                            eprintln!("error: git add after merge failed: {}", e);
                            process::exit(1);
                        }
                        if let Err(e) = git(&dir, &["commit", "-m", "luze sync: merge"]) {
                            eprintln!("error: git commit after merge failed: {}", e);
                            process::exit(1);
                        }
                    }
                    Err(e) => {
                        eprintln!("error: merge failed: {}", e);
                        process::exit(1);
                    }
                }
            } else {
                eprintln!("error: git pull failed: {}", e);
                process::exit(1);
            }
        }
    }

    // Step 3: push (set upstream on first push)
    let push_result = if tracking {
        git(&dir, &["push"])
    } else {
        git(&dir, &["push", "-u", &remote, &branch])
    };
    if let Err(e) = push_result {
        eprintln!("error: git push failed: {}", e);
        process::exit(1);
    }
}

fn sync_hint() {
    let dir = notes_dir();
    if !dir.join(".git").is_dir() || first_remote(&dir).is_none() { return; }
    let dirty = has_uncommitted(&dir);
    let ahead = unpushed_count(&dir);
    if dirty || ahead > 0 {
        let n = ahead + if dirty { 1 } else { 0 };
        eprintln!("hint: {} local change{} not synced. Run 'luze sync'", n, if n == 1 { "" } else { "s" });
    }
}

fn notes_dir() -> PathBuf {
    if let Ok(p) = env::var("LUZE_PATH") {
        return PathBuf::from(p);
    }
    let local = PathBuf::from("./.luze");
    if local.is_dir() {
        return local;
    }
    env::var("HOME").map(|h| PathBuf::from(h).join(".luze")).unwrap_or(local)
}

fn load_notes() -> NoteBox {
    let dir = notes_dir();
    NoteBox::open(&dir).unwrap_or_else(|e| {
        eprintln!("error: could not open {}: {}", dir.display(), e);
        process::exit(1);
    })
}

fn save_notes(notes: &NoteBox) {
    notes.save().unwrap_or_else(|e| {
        eprintln!("error: could not save: {}", e);
        process::exit(1);
    });
}

/// Returns the first line of content (the headline).
fn headline(content: &str) -> &str {
    content.lines().next().unwrap_or("")
}

/// Rejects single-line content longer than 150 characters.
/// Multi-line notes (headline + body) are always accepted.
fn validate_content(content: &str) {
    if !content.contains('\n') && content.chars().count() > 150 {
        eprintln!("error: content is a single line with more than 150 characters");
        eprintln!("hint:  add a newline after the headline to include a longer body");
        process::exit(1);
    }
}

fn print_help() {
    println!("luze — a digital Zettelkasten in the spirit of Luhmann.");
    println!();
    println!("Niklas Luhmann kept a box of handwritten notes (Zettel) that became his");
    println!("primary tool for thinking and writing. There are no categories or tags.");
    println!("Instead, each note gets a fixed position in a branching tree — you attach");
    println!("a new thought to the most relevant existing note, and topics emerge from");
    println!("the branches that grow. Cross-links connect related notes across distant");
    println!("parts of the tree. Over time, you may write a note that ties a cluster");
    println!("together — not as a predefined category, but as a summary of structure");
    println!("that has already grown.");
    println!();
    println!("A note must be atomic — one indivisible thought, as dense as possible,");
    println!("not exceeding 250 characters.");
    println!();
    println!("Notes have a hierarchical ID (e.g. 1a2b), immutable content, and links.");
    println!("New thoughts branch from existing notes; updates create a new child that");
    println!("supersedes the original. The first line of a note is its headline (max");
    println!("150 chars for single-line notes). Use search to find entry points, then");
    println!("navigate with show, children, backlinks, and ancestors.");
    println!();
    println!("Usage: luze <command> [args]");
    println!();
    println!("Commands:");
    println!("  init [--global]          Create .luze/ (--global creates ~/.luze/)");
    println!("  add <id> <content>      Add a note; parent derived from ID");
    println!("  update <id> <content>   Create a new child note that supersedes <id>");
    println!("  link <from> <to>        Add a link from one note to another");
    println!("  unlink <from> <to>      Remove a link between two notes");
    println!("  list [--all]            Print all notes (skip superseded unless --all)");
    println!("  show <id>               Print full content + links + version info");
    println!("  children <id>           List direct children of a note");
    println!("  ancestors <id>          Print breadcrumb path to a note");
    println!("  backlinks <id>          Notes that link to this note");
    println!("  search [--all] <query>  Case-insensitive search (skip superseded unless --all)");
    println!("  sync [-m <msg>]          Commit, pull, merge, and push via git");
    println!("  merge                   Auto-resolve git conflicts in draw files");
    println!("  tree [-d <depth>] [id]  Show subtree (all notes; [outdated]/[v2] markers)");
    println!("  help                    Show this message");
    println!();
    println!("Environment:");
    println!("  LUZE_PATH  Directory for the NoteBox");
    println!("             Resolved: LUZE_PATH > ./.luze (if exists) > ~/.luze");
}
