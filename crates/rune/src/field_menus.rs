//! Whether text fields open Rune's own menu (`runtime/input/field_menu.rs`: spelling
//! suggestions, Cut, Copy, Paste, Select All) on a right-click, the Menu key or Shift+F10
//! (AgentOps patch, PARITY-PATCH.md). On by default, as upstream. An app that draws its own
//! field menu from an ancestor's `on_secondary_press` (a wrapper around a kit field it can't
//! reach into) turns them off with [`set_field_menus`] before its window opens: a field's
//! secondary press then goes to the nearest `on_secondary_press`, as before field menus.
use std::sync::atomic::{AtomicBool, Ordering};

static FIELD_MENUS: AtomicBool = AtomicBool::new(true);

/// Turns text fields' own menus on (the default) or off for every window.
pub fn set_field_menus(on: bool) {
    FIELD_MENUS.store(on, Ordering::Relaxed);
}

/// Whether text fields open their own menu.
pub fn field_menus() -> bool {
    #[cfg(test)]
    if let Some(on) = FIELD_MENUS_HERE.with(std::cell::Cell::get) {
        return on;
    }
    FIELD_MENUS.load(Ordering::Relaxed)
}

#[cfg(test)]
std::thread_local! {
    /// `with_field_menus`: this thread's switch, over the process's.
    static FIELD_MENUS_HERE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Runs `f` with field menus `on` for this thread alone, so a test can flip them without
/// touching the tests running beside it.
#[cfg(test)]
pub(crate) fn with_field_menus<R>(on: bool, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<bool>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FIELD_MENUS_HERE.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(FIELD_MENUS_HERE.with(|c| c.replace(Some(on))));
    f()
}

#[cfg(test)]
mod tests {
    use crate::{Cx, El, Env, PointerPress, Runtime, TextEdit, Tx, el, text_input};

    #[derive(Default)]
    struct Form {
        edit: TextEdit,
        app_menus: u32,
    }

    /// A field inside a wrapper that opens the app's own menu, as an app's field menu does
    /// around a kit text input.
    fn view(s: &Form, _: &mut Cx<Form>) -> El {
        el().child(
            el().key("frame").size(220.0, 40.0)
                .on_secondary_press(|s: &mut Form, _: PointerPress, _: &mut Tx| s.app_menus += 1)
                .child(text_input(&s.edit, crate::tok::type_::BODY).key("field").single_line().w(200.0)
                    .on_edit(|s: &mut Form| &mut s.edit)),
        )
    }

    /// Right-clicks the field with field menus `on`: the wrapper's menus opened, and whether
    /// Rune's own menu asked to open.
    fn right_click(on: bool) -> (u32, bool) {
        super::with_field_menus(on, || {
            let mut rt = Runtime::new(Form::default(), view, (240.0, 60.0), Env::new(true, 1.0));
            rt.frame(0.0);
            let (x, y) = rt.find("field").expect("the field");
            rt.secondary_down(x, y);
            rt.secondary_up(x, y);
            rt.frame(0.0);
            (rt.state.app_menus, rt.take_popup().is_some())
        })
    }

    #[test]
    fn a_fields_own_menu_wins_unless_the_app_turns_field_menus_off() {
        assert_eq!(right_click(true), (0, true), "on (the default): Rune's field menu, over the wrapper's");
        assert_eq!(right_click(false), (1, false), "off: the wrapper's menu, as before field menus");
    }
}
