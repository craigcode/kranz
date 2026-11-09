// Kranz compatibility adapter for the GTK 3 macros. Only the two abort forms
// used by these crates are supported; syn owns span-aware compile_error output.
// A private unwind marker stops nested parsers without printing a panic hook.
use std::cell::RefCell;

thread_local! {
    static ERROR: RefCell<Option<syn::Error>> = const { RefCell::new(None) };
}

struct Abort;

pub(crate) fn abort_error(error: syn::Error) -> ! {
    ERROR.with(|slot| *slot.borrow_mut() = Some(error));
    std::panic::resume_unwind(Box::new(Abort))
}

pub(crate) fn run(f: impl FnOnce() -> proc_macro::TokenStream) -> proc_macro::TokenStream {
    let previous = ERROR.with(|slot| slot.borrow_mut().take());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    let error = ERROR.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), previous));
    match result {
        Ok(tokens) => tokens,
        Err(payload) if payload.is::<Abort>() => {
            error.expect("diagnostic abort carries an error").to_compile_error().into()
        }
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

macro_rules! abort_call_site {
    ($message:expr $(,)?) => {
        crate::diagnostics::abort_error(syn::Error::new(proc_macro2::Span::call_site(), $message))
    };
    ($format:literal, $($args:tt)*) => {
        abort_call_site!(format!($format, $($args)*))
    };
}
pub(crate) use abort_call_site;

macro_rules! abort {
    ($tokens:expr, $($message:tt)*) => {
        crate::diagnostics::abort_error(syn::Error::new_spanned(&$tokens, format!($($message)*)))
    };
}
pub(crate) use abort;
