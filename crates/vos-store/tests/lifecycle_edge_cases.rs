use std::path::Path;

use vos_core::util::now_unix;
use vos_core::{ObjectKind, ProjectHealth};
use vos_store::{Store, UpsertObjectInput};

#[test]
fn health_transitions_cover_empty_active_warm_and_dormant() {
    let mut store = Store::open_in_memory().unwrap();
    let now = now_unix();

    let empty = store.create_project("empty", None, None).unwrap();
    assert_eq!(
        store.project_status("empty").unwrap().health,
        ProjectHealth::Empty
    );
    let _ = empty;

    let warm = store.create_project("warm", None, None).unwrap();
    store
        .upsert_object(UpsertObjectInput {
            project_id: Some(warm.id),
            path: "/w/main.rs".to_string(),
            kind: ObjectKind::Code,
            title: "main.rs".to_string(),
            summary: None,
            content_hash: "hw".to_string(),
            size_bytes: 10,
            modified_at: now - 35 * 86_400,
            is_noise: false,
            chunks: Vec::new(),
        })
        .unwrap();
    assert_eq!(
        store.project_status("warm").unwrap().health,
        ProjectHealth::Warm
    );

    let dormant = store.create_project("dormant", None, None).unwrap();
    store
        .upsert_object(UpsertObjectInput {
            project_id: Some(dormant.id),
            path: "/d/main.rs".to_string(),
            kind: ObjectKind::Code,
            title: "main.rs".to_string(),
            summary: None,
            content_hash: "hd".to_string(),
            size_bytes: 10,
            modified_at: now - 90 * 86_400,
            is_noise: false,
            chunks: Vec::new(),
        })
        .unwrap();
    let st = store.project_status("dormant").unwrap();
    assert_eq!(st.health, ProjectHealth::Dormant);
    assert!(st.risk_areas.iter().any(|r| r.contains("project dormant")));
}

#[test]
fn unicode_and_cyrillic_content_indexes_and_searches_cleanly() {
    let mut store = Store::open_in_memory().unwrap();
    let proj = store.create_project("ru", None, None).unwrap();

    store
        .upsert_object(UpsertObjectInput {
            project_id: Some(proj.id),
            path: "/ru/Заметка.md".to_string(),
            kind: ObjectKind::Note,
            title: "Заметка.md".to_string(),
            summary: Some("Память проекта и планировщик".to_string()),
            content_hash: "h_ru".to_string(),
            size_bytes: 64,
            modified_at: now_unix(),
            is_noise: false,
            chunks: vec!["Архитектура локального индексатора.".to_string()],
        })
        .unwrap();

    let hits = store.search("индексатора", Some("ru"), 5).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].title, "Заметка.md");
}

#[test]
fn nested_projects_resolve_most_specific_root_path() {
    let tmp = tempfile::TempDir::new().unwrap();
    let outer = tmp.path().join("outer");
    let inner = outer.join("nested");
    std::fs::create_dir_all(&inner).unwrap();
    let target_file = inner.join("lib.rs");
    std::fs::write(&target_file, "pub fn x() {}").unwrap();

    let store = Store::open_in_memory().unwrap();
    store.create_project("outer", Some(&outer), None).unwrap();
    store.create_project("inner", Some(&inner), None).unwrap();

    let canonical = vos_core::util::canonicalize_path(Path::new(&target_file)).unwrap();
    let matched = store.find_project_for_path(&canonical).unwrap().unwrap();
    assert_eq!(matched.name, "inner");
}
