use crate::db::SolutionsDb;
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
