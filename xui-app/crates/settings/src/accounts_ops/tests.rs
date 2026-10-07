use super::*;
use crate::accounts::MemAccounts;

/// The accounts as the page lists them.
fn list(accounts: &MemAccounts) -> Vec<Account> {
    accounts.list().unwrap()
}

fn named(accounts: &MemAccounts, name: &str) -> Account {
    list(accounts)
        .into_iter()
        .find(|account| account.name == name)
        .unwrap()
}

/// `admin` runs Settings, beside `user` and a standard `bob`.
fn as_admin() -> MemAccounts {
    let accounts = MemAccounts {
        me: Some(String::from("admin")),
        ..MemAccounts::default()
    };
    accounts.create("bob", "bob-pass", false).unwrap();
    accounts
}

#[test]
fn nobody_removes_the_account_they_are_using() {
    let accounts = MemAccounts::default();
    let me = named(&accounts, "user");
    let refused = remove(&accounts, &list(&accounts), &me).unwrap_err();
    assert!(refused.contains("you are using"), "{refused}");
    assert_eq!(list(&accounts).len(), 2);
}

#[test]
fn the_last_administrator_stays() {
    let accounts = MemAccounts::default();
    let admin = named(&accounts, "admin");
    let all = list(&accounts);
    let refused = remove(&accounts, &all, &admin).unwrap_err();
    assert!(refused.contains("last administrator"), "{refused}");
    let refused = set_admin(&accounts, &all, &admin, false).unwrap_err();
    assert!(refused.contains("last administrator"), "{refused}");
    assert!(named(&accounts, "admin").admin);
    // With a second administrator the first may go.
    set_admin(&accounts, &all, &named(&accounts, "user"), true).unwrap();
    let all = list(&accounts);
    assert!(set_admin(&accounts, &all, &admin, false).is_ok());
    assert!(!named(&accounts, "admin").admin);
}

#[test]
fn an_administrator_removes_and_promotes_others() {
    let accounts = as_admin();
    let bob = named(&accounts, "bob");
    let text = set_admin(&accounts, &list(&accounts), &bob, true).unwrap();
    assert_eq!(text, "bob is an administrator.");
    let text = set_admin(&accounts, &list(&accounts), &named(&accounts, "bob"), true).unwrap();
    assert!(text.contains("already"), "{text}");
    let text = remove(&accounts, &list(&accounts), &named(&accounts, "bob")).unwrap();
    assert!(text.contains("archived"), "{text}");
    assert!(list(&accounts).iter().all(|a| a.name != "bob"));
}

#[test]
fn an_administrator_sets_another_password_typed_twice() {
    let accounts = as_admin();
    let bob = named(&accounts, "bob");
    for (password, again, why) in [
        ("abc", "abc", "4 to 64"),
        ("new-pass", "new-pas", "differ"),
        ("tab\there", "tab\there", "control"),
    ] {
        let refused = set_password(&accounts, &bob, password, again).unwrap_err();
        assert!(refused.contains(why), "{refused}");
    }
    assert_eq!(accounts.password("bob").as_deref(), Some("bob-pass"));
    set_password(&accounts, &bob, "new-pass", "new-pass").unwrap();
    assert_eq!(accounts.password("bob").as_deref(), Some("new-pass"));
    // One's own password needs the current one: not through this path.
    let me = named(&accounts, "admin");
    let refused = set_password(&accounts, &me, "new-pass", "new-pass").unwrap_err();
    assert!(refused.contains("current one"), "{refused}");
}

#[test]
fn add_checks_the_name_and_the_password_first() {
    let accounts = as_admin();
    for (name, password, again, why) in [
        ("Bad Name", "secret-1", "secret-1", "lowercase"),
        ("_svc", "secret-1", "secret-1", "lowercase"),
        ("root", "secret-1", "secret-1", "root"),
        ("carol", "abc", "abc", "4 to 64"),
        ("carol", "secret-1", "secret-2", "differ"),
    ] {
        let refused = add(&accounts, name, password, again, false).unwrap_err();
        assert!(refused.contains(why), "{name}: {refused}");
    }
    assert_eq!(list(&accounts).len(), 3);
    let text = add(&accounts, "  carol ", "secret-1", "secret-1", true).unwrap();
    assert_eq!(text, "carol was added.");
    assert!(named(&accounts, "carol").admin);
    assert!(add(&accounts, "carol", "secret-1", "secret-1", false).is_err());
}

#[test]
fn a_refused_prompt_changes_nothing() {
    let accounts = as_admin();
    *accounts.refuse.borrow_mut() = Some(String::from("cancelled"));
    let all = list(&accounts);
    let bob = named(&accounts, "bob");
    assert_eq!(remove(&accounts, &all, &bob), Err("cancelled".into()));
    assert_eq!(
        set_admin(&accounts, &all, &bob, true),
        Err("cancelled".into())
    );
    assert_eq!(
        set_password(&accounts, &bob, "new-pass", "new-pass"),
        Err("cancelled".into())
    );
    assert_eq!(
        add(&accounts, "carol", "secret-1", "secret-1", false),
        Err("cancelled".into())
    );
    assert_eq!(list(&accounts), all);
    assert_eq!(accounts.password("bob").as_deref(), Some("bob-pass"));
}

#[test]
fn your_own_password_needs_the_current_one_and_the_rule() {
    let accounts = MemAccounts::default();
    let refused = change_own(&accounts, "lazy", "abc", "abc").unwrap_err();
    assert!(refused.contains("4 to 64"), "{refused}");
    let refused = change_own(&accounts, "wrong", "longer-1", "longer-1").unwrap_err();
    assert!(refused.contains("wrong"), "{refused}");
    assert_eq!(
        change_own(&accounts, "lazy", "longer-1", "longer-1").as_deref(),
        Ok("Your password was changed.")
    );
    assert_eq!(accounts.password("user").as_deref(), Some("longer-1"));
}
