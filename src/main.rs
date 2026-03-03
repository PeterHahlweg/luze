use std::{collections::HashSet, env, path::PathBuf, process};
use luze::{ID, Note, NoteBox, MergeAction, merge_conflicts};

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_help();
        process::exit(1);
    }

    match args[1].as_str() {
        "init" => {
            let notes = NoteBox::create(notes_dir());
            save_notes(&notes);
        }

        "add" => {
            if args.len() < 4 {
                eprintln!("error: usage: luze add <id> <content>");
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
                process::exit(1);
            }
            save_notes(&notes);
        }

        "update" => {
            if args.len() < 4 {
                eprintln!("error: usage: luze update <id> <content>");
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
                }
                Err(e) => {
                    eprintln!("error: {}", e);
                    process::exit(1);
                }
            }
        }

        "unlink" => {
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

        "link" => {
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
        }

        "list" => {
            let show_all = args.iter().any(|a| a == "--all");
            let mut notes = load_notes();
            notes.load_all().unwrap_or_else(|e| { eprintln!("error: {}", e); process::exit(1) });
            let superseded: HashSet<&ID> = if show_all { HashSet::new() } else { notes.superseded_ids() };
            for note in notes.notes() {
                if !show_all && superseded.contains(note.id()) { continue; }
                println!("{:<6} {}", note.id(), headline(note.content()));
            }
        }

        "show" => {
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

        "children" => {
            if args.len() < 3 {
                eprintln!("error: usage: luze children <id>");
                process::exit(1);
            }
            let id = ID::from(args[2].as_str());
            let mut notes = load_notes();
            let notes = notes.children(&id)
                .unwrap_or_else(|e| { eprintln!("error: {}", e); process::exit(1) });
            for note in notes {
                println!("{:<6} {}", note.id(), headline(note.content()));
            }
        }

        "ancestors" => {
            if args.len() < 3 {
                eprintln!("error: usage: luze ancestors <id>");
                process::exit(1);
            }
            let id = ID::from(args[2].as_str());
            let mut notes = load_notes();
            let notes = notes.ancestors(&id)
                .unwrap_or_else(|e| { eprintln!("error: {}", e); process::exit(1) });
            for note in &notes {
                println!("{:<6} {}", note.id(), headline(note.content()));
            }
        }

        "backlinks" => {
            if args.len() < 3 {
                eprintln!("error: usage: luze backlinks <id>");
                process::exit(1);
            }
            let id = ID::from(args[2].as_str());
            let mut notes = load_notes();
            let notes = notes.backlinks(&id)
                .unwrap_or_else(|e| { eprintln!("error: {}", e); process::exit(1) });
            for note in notes {
                println!("{:<6} {}", note.id(), headline(note.content()));
            }
        }

        "search" => {
            if args.len() < 3 {
                eprintln!("error: usage: luze search <query>");
                process::exit(1);
            }
            let show_all = args.iter().any(|a| a == "--all");
            let query: String = args[2..].iter()
                .filter(|a| a.as_str() != "--all")
                .cloned().collect::<Vec<_>>().join(" ");
            let mut notes = load_notes();
            let notes = if show_all {
                notes.search_all(&query)
            } else {
                notes.search(&query)
            }.unwrap_or_else(|e| { eprintln!("error: {}", e); process::exit(1) });
            for note in notes {
                println!("{:<6} {}", note.id(), headline(note.content()));
            }
        }

        "tree" => {
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

        "merge" => {
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

        "help" | "--help" | "-h" => print_help(),

        cmd => {
            eprintln!("error: unknown command '{}'", cmd);
            process::exit(1);
        }
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

fn notes_dir() -> PathBuf {
    env::var("LUZE_PATH").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("./.luze"))
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
    println!("Notes have a hierarchical ID (e.g. 1a2b), immutable content, and links.");
    println!("New thoughts branch from existing notes; updates create a new child that");
    println!("supersedes the original. The first line of a note is its headline (max");
    println!("150 chars for single-line notes). Use search to find entry points, then");
    println!("navigate with show, children, backlinks, and ancestors.");
    println!();
    println!("Usage: luze <command> [args]");
    println!();
    println!("Commands:");
    println!("  init                    Create .luze/draws/ directory");
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
    println!("  merge                   Auto-resolve git conflicts in draw files");
    println!("  tree [-d <depth>] [id]  Show subtree (all notes; [outdated]/[v2] markers)");
    println!("  help                    Show this message");
    println!();
    println!("Environment:");
    println!("  LUZE_PATH  Directory for the NoteBox (default: ./.luze)");
}
