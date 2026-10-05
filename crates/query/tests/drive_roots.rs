//! Volume roots (`C:`) are results too: the user searches for a drive to open it.

use bs_index::IndexBuilder;
use bs_query::{Query, search};

fn index() -> bs_index::Index {
    let mut b = IndexBuilder::new();
    b.begin_volume("C:", 5);
    b.push(10, 5, "readme.txt", false, false);
    b.push(11, 5, "notes", true, false);
    b.push(12, 5, "calculator.lnk", false, false);
    b.push(13, 5, "c.png", false, false);
    b.end_volume();
    b.begin_volume("D:", 7);
    b.push(20, 7, "data", true, false);
    b.end_volume();
    b.finish()
}

fn hits(index: &bs_index::Index, query: &str) -> Vec<String> {
    let q = Query::parse(query).unwrap();
    search(index, &q, 20)
        .hits
        .iter()
        .map(|h| index.full_path(h.entry))
        .collect()
}

#[test]
fn a_letter_or_letter_colon_finds_the_drives() {
    let index = index();
    for query in ["c", "c:", "C:", "d", "d:"] {
        let found = hits(&index, query);
        assert!(
            found.iter().any(|p| p == r"C:\" || p == r"D:\"),
            "no drive root in hits for {query:?}: {found:?}"
        );
    }
    // The exact `c:` names the drive first: it is what the query means.
    assert_eq!(hits(&index, "c:")[0], r"C:\");
}

#[test]
fn a_root_needs_its_own_name_to_carry_every_term() {
    let index = index();
    // Two terms: the root's name only has "c", and it has no folders above it to
    // supply "readme", so it must not appear.
    assert!(!hits(&index, "c readme").iter().any(|p| p == r"C:\"));
    // But it does appear when the name carries both.
    assert!(hits(&index, "c: c").iter().any(|p| p == r"C:\"));
}

#[test]
fn a_root_is_hidden_with_system_folders_and_in_scope_rules() {
    let index = index();
    // Hidden system and app folders are a window option, not a search rule; roots
    // are normal folders, so only the scope filter can exclude one.
    let q = Query::parse("c").unwrap();
    let result = search(&index, &q, 20);
    assert_eq!(result.hidden_matches, 0);
    // Scoped to D:, the C: root is out of scope, and D: itself is not below its own
    // scope ("in:" keeps entries below the folder).
    let scoped = hits(&index, r#"d in:"D:\"#);
    assert!(scoped.iter().all(|p| p != r"C:\"));
    assert!(scoped.iter().all(|p| p != r"D:\"));
    assert!(scoped.iter().any(|p| p == r"D:\data"));
}
