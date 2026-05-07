use crate::db::{PanelKind, SolutionsDb};
use crate::model::CatalogId;
use gpui::TestAppContext;

#[gpui::test]
async fn store_loads_catalog_and_solutions_from_db(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let db = SolutionsDb::open_test_db("solutions_store_e2e_init").await;
    db.save_catalog_project("cat-a".into(), "Cat A".into(), "git@x:a".into(), None)
        .await
        .unwrap();
    db.save_solution("s1".into(), "Sol 1".into(), "/tmp/s1".into(), None)
        .await
        .unwrap();
    db.set_solution_member("s1".into(), "cat-a".into(), "/tmp/s1/cat-a".into(), 0)
        .await
        .unwrap();

    let db_for_init = db.clone();
    cx.update(|cx| {
        crate::store::SolutionStore::init_global_for_test(db_for_init, cx);
        let store = crate::store::SolutionStore::global(cx);
        store.read_with(cx, |s, _| {
            assert_eq!(s.catalog().len(), 1);
            assert_eq!(s.catalog()[0].id.as_str(), "cat-a");
            assert_eq!(s.solutions().len(), 1);
            assert_eq!(s.solutions()[0].id.as_str(), "s1");
            assert_eq!(s.solutions()[0].members.len(), 1);
            assert_eq!(s.solutions()[0].members[0].catalog_id.as_str(), "cat-a");
        });
    });
}

#[gpui::test]
async fn add_catalog_project_persists_to_db(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let db = SolutionsDb::open_test_db("solutions_store_e2e_add_catalog").await;
    let db_for_init = db.clone();
    cx.update(|cx| {
        crate::store::SolutionStore::init_global_for_test(db_for_init, cx);
        let store = crate::store::SolutionStore::global(cx);
        store.update(cx, |s, cx| {
            s.add_catalog_project("Foo", "git@x:foo.git", Some("main".into()), cx)
                .unwrap();
        });
    });

    let rows = db.load_all_catalog_projects().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "Foo");
}

#[gpui::test]
async fn create_and_remove_solution_persists(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let db = SolutionsDb::open_test_db("solutions_store_e2e_create_remove").await;
    let tmp = tempfile::tempdir().unwrap();
    let db_for_init = db.clone();
    cx.update(|cx| {
        crate::store::SolutionStore::init_global_for_test(db_for_init, cx);
        let store = crate::store::SolutionStore::global(cx);
        let id = store
            .update(cx, |s, cx| s.create_solution("My Sol", tmp.path().to_path_buf(), cx).unwrap());
        store.update(cx, |s, cx| s.touch_last_opened(&id, cx).unwrap());
        store.update(cx, |s, cx| s.delete_solution(&id, cx).unwrap());
    });

    let rows = db.load_all_solutions_with_members().await.unwrap();
    assert!(rows.is_empty(), "delete_solution should remove the row");
}

#[gpui::test]
async fn touch_last_opened_persists_timestamp(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let db = SolutionsDb::open_test_db("solutions_store_e2e_touch_last").await;
    let tmp = tempfile::tempdir().unwrap();
    let db_for_init = db.clone();
    cx.update(|cx| {
        crate::store::SolutionStore::init_global_for_test(db_for_init, cx);
        let store = crate::store::SolutionStore::global(cx);
        let id = store
            .update(cx, |s, cx| s.create_solution("S", tmp.path().to_path_buf(), cx).unwrap());
        store.update(cx, |s, cx| s.touch_last_opened(&id, cx).unwrap());
    });
    let rows = db.load_all_solutions_with_members().await.unwrap();
    assert!(rows.iter().any(|r| r.3.is_some()), "last_opened_at should be set");
}

#[gpui::test]
async fn set_panel_selection_persists_and_emits(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let db = SolutionsDb::open_test_db("solutions_store_e2e_panel_sel").await;
    let tmp = tempfile::tempdir().unwrap();
    let db_for_init = db.clone();
    let id = cx.update(|cx| {
        crate::store::SolutionStore::init_global_for_test(db_for_init, cx);
        let store = crate::store::SolutionStore::global(cx);
        store.update(cx, |s, cx| s.create_solution("S", tmp.path().to_path_buf(), cx).unwrap())
    });
    cx.update(|cx| {
        let store = crate::store::SolutionStore::global(cx);
        store.update(cx, |s, cx| {
            s.set_panel_member_selection(
                id.clone(),
                PanelKind::Tree,
                CatalogId("cat-x".into()),
                cx,
            )
            .unwrap();
        });
    });

    let rows = db.load_all_panel_selections().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "tree");
    assert_eq!(rows[0].2, "cat-x");
}

use std::fs;

#[gpui::test]
async fn migration_imports_old_json_once(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let tmp = tempfile::tempdir().unwrap();
    let json_path = tmp.path().join("solutions.json");

    let old = serde_json::json!({
        "version": 1,
        "catalog": [
            { "id": "cat-a", "name": "Cat A", "remote_url": "git@x:a", "default_branch": "main" }
        ],
        "solutions": [
            {
                "id": "sol-1",
                "name": "Sol One",
                "root": "/tmp/sol-1",
                "members": [
                    { "catalog_id": "cat-a", "local_path": "/tmp/sol-1/cat-a" }
                ]
            }
        ]
    });
    fs::write(&json_path, serde_json::to_string_pretty(&old).unwrap()).unwrap();

    let db = SolutionsDb::open_test_db("solutions_migrate_imports").await;
    crate::migrate::run_one_time_migration(&db, &json_path).unwrap();

    let cat = db.load_all_catalog_projects().await.unwrap();
    assert_eq!(cat.len(), 1);
    let sol = db.load_all_solutions_with_members().await.unwrap();
    assert_eq!(sol.len(), 1);
    assert_eq!(sol[0].4, "cat-a");

    assert!(!json_path.exists());
    let bak = tmp.path().join("solutions.json.migrated.bak");
    assert!(bak.exists());

    crate::migrate::run_one_time_migration(&db, &json_path).unwrap();
    let cat = db.load_all_catalog_projects().await.unwrap();
    assert_eq!(cat.len(), 1);
}
