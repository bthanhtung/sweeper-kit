mod common;

use std::os::unix::fs::symlink;
use std::path::PathBuf;

use common::*;
use sweeper_core::testing::{make_app, FakeSpotlight, Fixture};
use sweeper_core::{PathPolicy, PolicyError, SELF_BUNDLE_ID};

fn policy(fx: &Fixture) -> PathPolicy {
    PathPolicy::new(&fx.env, &[])
}

#[test]
fn allows_leftovers_and_apps_and_returns_canonical_path() {
    let fx = Fixture::new();
    let app = fx.install("Foo", "com.foo.app", "1.0");
    let cache = touch(&fx, "Library/Caches/com.foo.app");
    let p = policy(&fx);
    assert_eq!(p.check(&app).unwrap(), app);
    assert_eq!(p.check(&cache).unwrap(), cache);
}

#[test]
fn roots_themselves_and_outside_paths_are_refused() {
    let fx = Fixture::new();
    let p = policy(&fx);
    assert_eq!(
        p.check(&fx.lib("Caches")),
        Err(PolicyError::OutsideAllowedRoots)
    );
    assert_eq!(p.check(&fx.apps_dir), Err(PolicyError::OutsideAllowedRoots));
    let stray = touch(&fx, "Library/Fonts/x.ttf");
    assert_eq!(p.check(&stray), Err(PolicyError::OutsideAllowedRoots));
    assert_eq!(
        p.check(&PathBuf::from("relative/path")),
        Err(PolicyError::OutsideAllowedRoots)
    );
    assert_eq!(
        p.check(&fx.lib("Caches").join("..").join("Preferences")),
        Err(PolicyError::OutsideAllowedRoots)
    );
    assert_eq!(
        p.check(&fx.lib("Caches").join("missing")),
        Err(PolicyError::NotFound)
    );
}

#[test]
fn nested_app_dir_root_cannot_be_removed() {
    let mut fx = Fixture::new();
    let utilities = fx.apps_dir.join("Utilities");
    std::fs::create_dir_all(&utilities).unwrap();
    fx.env.app_dirs.push(utilities.clone());
    assert_eq!(
        policy(&fx).check(&utilities),
        Err(PolicyError::OutsideAllowedRoots)
    );
}

#[test]
fn symlink_to_outside_is_refused_and_not_removed() {
    let fx = Fixture::new();
    fx.install("Foo", "com.foo.app", "1.0");
    let outside = outside_dir(&fx);
    let link = fx.lib("Caches").join("com.foo.app");
    symlink(&outside, &link).unwrap();

    assert_eq!(policy(&fx).check(&link), Err(PolicyError::SymlinkEscape));

    let result = scan(&fx, "com.foo.app");
    let item = result
        .items()
        .find(|i| i.path == link)
        .expect("symlink listed");
    assert!(!item.checked);
    assert!(item.refused.as_deref().unwrap().contains("symbolic link"));
}

#[test]
fn symlink_inside_allowed_roots_is_accepted() {
    let fx = Fixture::new();
    let real = touch_dir(&fx, "Library/Application Support/real");
    let link = fx.lib("Caches").join("alias");
    symlink(&real, &link).unwrap();
    assert_eq!(policy(&fx).check(&link).unwrap(), link);
}

#[test]
fn symlinked_parent_directory_is_refused() {
    let fx = Fixture::new();
    let outside = outside_dir(&fx);
    let linked = fx.lib("Caches").join("linked");
    symlink(&outside, &linked).unwrap();
    assert_eq!(
        policy(&fx).check(&linked.join("precious.txt")),
        Err(PolicyError::SymlinkEscape)
    );
    assert!(outside.join("precious.txt").exists());
}

#[test]
fn dangling_symlink_is_refused() {
    let fx = Fixture::new();
    let link = fx.lib("Caches").join("dangling");
    symlink(fx.tmp.path().join("nowhere"), &link).unwrap();
    assert_eq!(policy(&fx).check(&link), Err(PolicyError::SymlinkEscape));
}

#[test]
fn protected_paths_and_prefixes_are_refused() {
    let fx = Fixture::new();
    let dir = touch_dir(&fx, "Library/Application Support/Keep");
    let inner = dir.join("data.bin");
    let other = touch(&fx, "Library/Application Support/Keeper");
    let p = PathPolicy::new(&fx.env, &[dir.display().to_string()]);
    assert_eq!(p.check(&dir), Err(PolicyError::Protected));
    assert_eq!(p.check(&inner), Err(PolicyError::Protected));
    assert!(
        p.check(&other).is_ok(),
        "prefix match is per path component"
    );
}

#[test]
fn protected_bundle_id_is_refused() {
    let fx = Fixture::new();
    let app = fx.install("Secret", "com.corp.secret", "1.0");
    let p = PathPolicy::new(&fx.env, &["com.corp.secret".to_string()]);
    assert_eq!(p.check(&app), Err(PolicyError::Protected));

    let result = scan_with(
        &fx,
        "com.corp.secret",
        &["com.corp.secret".to_string()],
        &FakeSpotlight::default(),
    );
    let item = &result.groups[0].items[0];
    assert!(!item.checked && item.refused.is_some());
}

#[test]
fn apple_apps_are_read_only() {
    let fx = Fixture::new();
    let app = fx.install("Calc", "com.apple.calculator", "1.0");
    touch(&fx, "Library/Preferences/com.apple.calculator.plist");
    assert!(matches!(
        policy(&fx).check(&app),
        Err(PolicyError::Denied(_))
    ));

    let result = scan(&fx, "com.apple.calculator");
    assert!(result.app.is_apple);
    assert!(result.items().count() >= 2);
    for item in result.items() {
        assert!(!item.checked);
        assert_eq!(item.refused.as_deref(), Some("Apple app — read only"));
    }
}

#[test]
fn sweeper_kit_itself_and_its_data_are_refused() {
    let fx = Fixture::new();
    let me = fx.install("Sweeper Kit", SELF_BUNDLE_ID, "1.0");
    let p = policy(&fx);
    assert!(matches!(p.check(&me), Err(PolicyError::Denied(_))));

    let data = touch(&fx, "Library/Application Support/Sweeper Kit/undo-log.json");
    assert!(matches!(p.check(&data), Err(PolicyError::Denied(_))));
    assert!(matches!(
        p.check(data.parent().unwrap()),
        Err(PolicyError::Denied(_))
    ));
}

#[test]
fn personal_and_private_folders_are_denied() {
    let fx = Fixture::new();
    let p = policy(&fx);
    for rel_path in [
        "Documents/report.docx",
        "Desktop/a.txt",
        "Pictures/b.png",
        "Movies/c.mov",
        "Music/d.mp3",
        "Downloads/e.zip",
        "Library/Keychains/login.keychain-db",
        "Library/Mail/V10/x",
        "Library/Messages/chat.db",
        "Library/Mobile Documents/com~apple~CloudDocs/f",
    ] {
        let path = touch(&fx, rel_path);
        assert!(
            matches!(p.check(&path), Err(PolicyError::Denied(_))),
            "{rel_path} must be denied"
        );
    }
}

#[test]
fn apple_containers_and_photos_libraries_are_denied() {
    let fx = Fixture::new();
    let p = policy(&fx);
    for rel_path in [
        "Library/Containers/com.apple.Notes/Data/x",
        "Library/Group Containers/group.com.apple.notes/x",
        "Library/Group Containers/ABCDE12345.com.apple.mail/x",
        "Library/Application Support/Library.photoslibrary/originals/x",
    ] {
        let path = touch(&fx, rel_path);
        assert!(
            matches!(p.check(&path), Err(PolicyError::Denied(_))),
            "{rel_path} must be denied"
        );
    }
    let third_party = touch(&fx, "Library/Containers/com.foo.app/Data/x");
    assert!(p.check(&third_party).is_ok());
}

#[test]
fn app_with_apple_id_in_unusual_dir_is_denied_even_inside_bundle() {
    let fx = Fixture::new();
    let app = make_app(&fx.apps_dir, "Pages", "com.apple.iWork.Pages", "1.0");
    let inner = app.join("Contents/MacOS/Pages");
    assert!(matches!(
        policy(&fx).check(&inner),
        Err(PolicyError::Denied(_))
    ));
}

// ---- extra roots ----

fn extra(fx: &Fixture, roots: &[&str]) -> PathPolicy {
    let paths: Vec<PathBuf> = roots.iter().map(|r| fx.env.home.join(r)).collect();
    policy(fx).with_extra_roots(&paths)
}

#[test]
fn downloads_child_is_allowed_only_with_the_downloads_extra_root() {
    let fx = Fixture::new();
    let file = touch(&fx, "Downloads/old.zip");
    assert!(matches!(
        policy(&fx).check(&file),
        Err(PolicyError::Denied(_))
    ));

    let p = extra(&fx, &["Downloads"]);
    assert!(p.rejected_extra_roots().is_empty());
    assert_eq!(p.check(&file).unwrap(), file);
    // The root itself stays protected.
    assert_eq!(
        p.check(&fx.env.home.join("Downloads")),
        Err(PolicyError::OutsideAllowedRoots)
    );
}

#[test]
fn personal_folders_stay_denied_and_their_extra_roots_rejected() {
    let fx = Fixture::new();
    let doc = touch(&fx, "Documents/thesis.txt");
    let p = extra(
        &fx,
        &["Documents", "Desktop", "Library/Keychains", "Library/Mail"],
    );
    assert_eq!(p.rejected_extra_roots().len(), 4);
    assert!(matches!(p.check(&doc), Err(PolicyError::Denied(_))));
}

#[test]
fn extra_roots_containing_hard_denied_areas_are_rejected() {
    let fx = Fixture::new();
    let p = extra(&fx, &["", "Library", "Library/Application Support"]);
    assert_eq!(p.rejected_extra_roots().len(), 3);
    let stray = touch(&fx, "Library/Fonts/x.ttf");
    assert_eq!(p.check(&stray), Err(PolicyError::OutsideAllowedRoots));
}

#[test]
fn extra_root_outside_home_is_rejected() {
    let fx = Fixture::new();
    let outside = outside_dir(&fx);
    let roots = [outside.clone(), PathBuf::from("/usr/local")];
    let p = policy(&fx).with_extra_roots(&roots);
    assert_eq!(p.rejected_extra_roots(), roots);
    assert_eq!(
        p.check(&outside.join("precious.txt")),
        Err(PolicyError::OutsideAllowedRoots)
    );
}

#[test]
fn extra_root_that_is_a_symlink_out_of_home_is_rejected() {
    let fx = Fixture::new();
    let outside = outside_dir(&fx);
    symlink(&outside, fx.env.home.join(".npm")).unwrap();
    let p = extra(&fx, &[".npm"]);
    assert_eq!(p.rejected_extra_roots().len(), 1);
    assert_eq!(
        p.check(&outside.join("precious.txt")),
        Err(PolicyError::OutsideAllowedRoots)
    );
}

#[test]
fn symlink_inside_extra_root_pointing_outside_is_refused() {
    let fx = Fixture::new();
    let outside = outside_dir(&fx);
    let cache = fx.env.home.join(".npm/_cacache");
    touch(&fx, ".npm/_cacache/ok.bin");
    let p = extra(&fx, &[".npm"]);
    assert!(p.check(&cache).is_ok());

    let link = cache.join("escape");
    symlink(&outside, &link).unwrap();
    assert_eq!(p.check(&link), Err(PolicyError::SymlinkEscape));
    assert_eq!(
        p.check(&link.join("precious.txt")),
        Err(PolicyError::SymlinkEscape)
    );

    let docs = fx.env.home.join("Documents");
    std::fs::create_dir_all(&docs).unwrap();
    let to_docs = cache.join("to_docs");
    symlink(&docs, &to_docs).unwrap();
    assert_eq!(p.check(&to_docs), Err(PolicyError::SymlinkEscape));
}

#[test]
fn extra_roots_keep_the_other_checks() {
    let fx = Fixture::new();
    let p = extra(&fx, &["Downloads", ".npm"]);
    touch(&fx, "Downloads/Vacation.photoslibrary/db");
    let photos = fx.env.home.join("Downloads/Vacation.photoslibrary/db");
    assert!(matches!(p.check(&photos), Err(PolicyError::Denied(_))));
    let apple = make_app(
        &fx.env.home.join("Downloads"),
        "Maps",
        "com.apple.Maps",
        "1",
    );
    assert!(matches!(p.check(&apple), Err(PolicyError::Denied(_))));
    let own = make_app(&fx.env.home.join(".npm"), "Self", SELF_BUNDLE_ID, "1");
    assert!(matches!(p.check(&own), Err(PolicyError::Denied(_))));

    let keep = touch(&fx, ".npm/keep/file");
    let protected = PathPolicy::new(&fx.env, &["~/.npm/keep".to_string()])
        .with_extra_roots(&[fx.env.home.join(".npm")]);
    assert_eq!(protected.check(&keep), Err(PolicyError::Protected));
    // Without the extra root nothing changes.
    assert_eq!(
        policy(&fx).check(&keep),
        Err(PolicyError::OutsideAllowedRoots)
    );
}
