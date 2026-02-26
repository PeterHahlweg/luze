use std::{env, path::PathBuf, process};
use luze::{ID, Note, NoteBox, RemoveOutcome, MergeAction, merge_conflicts};

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_help();
        process::exit(1);
    }

    match args[1].as_str() {
        "init" => {
            let zk = NoteBox::create(zk_dir());
            save_zk(&zk);
        }

        "add" => {
            if args.len() < 4 {
                eprintln!("error: usage: zk add <id> <content>");
                process::exit(1);
            }
            let id = ID::from(args[2].as_str());
            let content = args[3..].join(" ");
            let mut zk = load_zk();
            let parent = id.parent();
            if parent != id {
                match zk.find(&parent) {
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
            if let Err(e) = zk.add(Note::new(id, parent, &content)) {
                eprintln!("error: {}", e);
                process::exit(1);
            }
            save_zk(&zk);
        }

        "update" => {
            if args.len() < 4 {
                eprintln!("error: usage: zk update <id> <content>");
                process::exit(1);
            }
            let id = ID::from(args[2].as_str());
            let content = args[3..].join(" ");
            let mut zk = load_zk();
            match zk.archive_update(&id, &content) {
                Ok(Some(result)) => {
                    save_zk(&zk);
                    println!("archived → {}", result.archive_id);
                    if !result.backlink_ids.is_empty() {
                        println!("backlinks to review:");
                        for bid in &result.backlink_ids {
                            println!("  {}", bid);
                        }
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

        "fix" => {
            if args.len() < 4 {
                eprintln!("error: usage: zk fix <id> <content>");
                process::exit(1);
            }
            let id = ID::from(args[2].as_str());
            let content = args[3..].join(" ");
            let mut zk = load_zk();
            match zk.find_mut(&id) {
                Ok(Some(note)) => { note.set_content(&content); save_zk(&zk); }
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

        "remove" => {
            if args.len() < 3 {
                eprintln!("error: usage: zk remove <id>");
                process::exit(1);
            }
            let id = ID::from(args[2].as_str());
            let mut zk = load_zk();
            match zk.remove(&id) {
                Ok(Some(RemoveOutcome::HasChildren(children))) => {
                    let list: Vec<String> = children.iter().map(|i| i.to_string()).collect();
                    eprintln!("error: {} has children: {}", id, list.join(", "));
                    eprintln!("hint:  use 'zk update {}' to evolve this note instead", id);
                    process::exit(1);
                }
                Ok(Some(RemoveOutcome::BacklinkCleared(from))) => {
                    save_zk(&zk);
                    println!("review: {} no longer links to {} — does its content still make sense?\n", from, id);
                    if let Ok(Some(note)) = zk.find(&from) {
                        println!("ID:      {}", note.id());
                        println!("Content: {}", note.content());
                        let links = note.links();
                        if !links.is_empty() {
                            let joined: Vec<String> = links.iter().map(|l| l.to_string()).collect();
                            println!("Links:   {}", joined.join(", "));
                        }
                    }
                    println!("\ncontinue: zk remove {}", id);
                }
                Ok(Some(RemoveOutcome::Removed(_))) => {
                    save_zk(&zk);
                    println!("removed {}", id);
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

        "unlink" => {
            if args.len() < 4 {
                eprintln!("error: usage: zk unlink <from> <to>");
                process::exit(1);
            }
            let from = ID::from(args[2].as_str());
            let to   = ID::from(args[3].as_str());
            let mut zk = load_zk();
            match zk.find_mut(&from) {
                Ok(Some(note)) => {
                    if !note.remove_link(&to) {
                        eprintln!("error: no link from {} to {}", from, to);
                        process::exit(1);
                    }
                    save_zk(&zk);
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
                eprintln!("error: usage: zk link <from> <to>");
                process::exit(1);
            }
            let from = ID::from(args[2].as_str());
            let to   = ID::from(args[3].as_str());
            let mut zk = load_zk();
            match zk.find_mut(&from) {
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
            save_zk(&zk);
        }

        "list" => {
            let mut zk = load_zk();
            zk.load_all().unwrap_or_else(|e| { eprintln!("error: {}", e); process::exit(1) });
            for note in zk.notes() {
                let preview: String = note.content().chars().take(60).collect();
                println!("{:<6} {}", note.id(), preview);
            }
        }

        "show" => {
            if args.len() < 3 {
                eprintln!("error: usage: zk show <id>");
                process::exit(1);
            }
            let id = ID::from(args[2].as_str());
            let mut zk = load_zk();
            match zk.find(&id) {
                Ok(Some(note)) => {
                    println!("ID:      {}", note.id());
                    println!("Created: {}", note.created_at().format("%Y-%m-%d %H:%M:%S UTC"));
                    println!("Content: {}", note.content());
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
                eprintln!("error: usage: zk children <id>");
                process::exit(1);
            }
            let id = ID::from(args[2].as_str());
            let mut zk = load_zk();
            let notes = zk.children(&id)
                .unwrap_or_else(|e| { eprintln!("error: {}", e); process::exit(1) });
            for note in notes {
                let preview: String = note.content().chars().take(60).collect();
                println!("{:<6} {}", note.id(), preview);
            }
        }

        "ancestors" => {
            if args.len() < 3 {
                eprintln!("error: usage: zk ancestors <id>");
                process::exit(1);
            }
            let id = ID::from(args[2].as_str());
            let mut zk = load_zk();
            let notes = zk.ancestors(&id)
                .unwrap_or_else(|e| { eprintln!("error: {}", e); process::exit(1) });
            for note in &notes {
                let preview: String = note.content().chars().take(60).collect();
                println!("{:<6} {}", note.id(), preview);
            }
        }

        "backlinks" => {
            if args.len() < 3 {
                eprintln!("error: usage: zk backlinks <id>");
                process::exit(1);
            }
            let id = ID::from(args[2].as_str());
            let mut zk = load_zk();
            let notes = zk.backlinks(&id)
                .unwrap_or_else(|e| { eprintln!("error: {}", e); process::exit(1) });
            for note in notes {
                let preview: String = note.content().chars().take(60).collect();
                println!("{:<6} {}", note.id(), preview);
            }
        }

        "search" => {
            if args.len() < 3 {
                eprintln!("error: usage: zk search <query>");
                process::exit(1);
            }
            let query = args[2..].join(" ");
            let mut zk = load_zk();
            let notes = zk.search(&query)
                .unwrap_or_else(|e| { eprintln!("error: {}", e); process::exit(1) });
            for note in notes {
                let preview: String = note.content().chars().take(60).collect();
                println!("{:<6} {}", note.id(), preview);
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

            let mut zk = load_zk();
            zk.load_all().unwrap_or_else(|e| { eprintln!("error: {}", e); process::exit(1) });
            let all_notes = zk.notes();

            if let Some(ref id) = root_id {
                if all_notes.iter().all(|n| n.id() != id) {
                    eprintln!("error: note {} not found", id);
                    process::exit(1);
                }
                print_tree(&all_notes, id, 0, max_depth, "", true);
            } else {
                let roots: Vec<&Note> = all_notes.iter()
                    .filter(|n| n.parent().map_or(false, |p| p == n.id()))
                    .copied()
                    .collect();
                let last = roots.len().saturating_sub(1);
                for (i, root) in roots.iter().enumerate() {
                    print_tree(&all_notes, root.id(), 0, max_depth, "", i == last);
                }
            }
        }

        "merge" => {
            match merge_conflicts(&zk_dir()) {
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
                        let mut zk = load_zk();
                        for (orig, renamed) in &conflicts {
                            for (label, id) in [("head ", orig), ("their", renamed)] {
                                print!("\n  [{}] {}", label, id);
                                if let Ok(Some(note)) = zk.find(id) {
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

fn print_tree(all: &[&Note], id: &ID, depth: usize, max_depth: usize, prefix: &str, is_last: bool) {
    let note = all.iter().find(|n| n.id() == id);
    let preview: String = note
        .map_or("", |n| n.content())
        .chars().take(50).collect();
    let connector = if depth == 0 { "" } else if is_last { "└── " } else { "├── " };
    println!("{}{}{} {}", prefix, connector, id, preview);

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
        print_tree(all, child.id(), depth + 1, max_depth, &child_prefix, i == last);
    }
}

fn zk_dir() -> PathBuf {
    env::var("ZK_PATH").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("./.zk"))
}

fn load_zk() -> NoteBox {
    let dir = zk_dir();
    NoteBox::open(&dir).unwrap_or_else(|e| {
        eprintln!("error: could not open {}: {}", dir.display(), e);
        process::exit(1);
    })
}

fn save_zk(zk: &NoteBox) {
    zk.save().unwrap_or_else(|e| {
        eprintln!("error: could not save: {}", e);
        process::exit(1);
    });
}

fn print_help() {
    println!("Usage: zk <command> [args]");
    println!();
    println!("Commands:");
    println!("  init                    Create zk/draws/ directory");
    println!("  add <id> <content>      Add a note; parent derived from ID");
    println!("  update <id> <content>   Archive old content as a child (OUTDATED), set new content");
    println!("  fix    <id> <content>   In-place content fix (typos, rephrasing — no archive)");
    println!("  remove <id>             Remove a note (does not update backlinks)");
    println!("  link <from> <to>        Add a link from one note to another");
    println!("  unlink <from> <to>      Remove a link between two notes");
    println!("  list                    Print all notes (id + first 60 chars)");
    println!("  show <id>               Print full content + links");
    println!("  children <id>           List direct children of a note");
    println!("  ancestors <id>          Print breadcrumb path to a note");
    println!("  backlinks <id>          Notes that link to this note");
    println!("  search <query>          Case-insensitive content search");
    println!("  merge                   Auto-resolve git conflicts in draw files");
    println!("  tree [-d <depth>] [id]  Show subtree (default: all roots, unlimited depth)");
    println!("  help                    Show this message");
    println!();
    println!("Environment:");
    println!("  ZK_PATH  Directory for the NoteBox (default: ./.zk)");
}
