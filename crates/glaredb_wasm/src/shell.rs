use std::cell::RefCell;
use std::io;
use std::rc::Rc;

use glaredb_core::shell::lineedit::{KeyEvent, TermSize};
use glaredb_core::shell::{InteractiveShell, RawModeGuard, RawModeTerm, Shell, ShellSignal};
use glaredb_error::{DbError, OptionExt};
use js_sys::Function;
use tracing::{error, trace, warn};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{KeyboardEvent, window};

use crate::errors::Result;
use crate::runtime::{WasmExecutor, WasmSystemRuntime};
use crate::session::WasmSession;

#[wasm_bindgen]
extern "C" {
    /// Structurally typed terminal interface for outputting data including
    /// terminal codes.
    ///
    /// xterm.js mostly.
    #[derive(Debug)]
    pub type Terminal;

    #[wasm_bindgen(method)]
    pub fn write(this: &Terminal, text: &str);

    #[wasm_bindgen(method)]
    pub fn writeln(this: &Terminal, text: &str);

    #[wasm_bindgen(js_name = "IDisposable")]
    pub type Disposable;

    #[wasm_bindgen(method, js_name = "dispose")]
    pub fn dispose(this: &Disposable);

    pub type OnKeyEvent;

    #[wasm_bindgen(method, getter, js_name = "key")]
    pub fn key(this: &OnKeyEvent) -> String;

    #[wasm_bindgen(method, getter, js_name = "domEvent")]
    pub fn dom_event(this: &OnKeyEvent) -> KeyboardEvent;

    #[wasm_bindgen(method, js_name = "onKey")]
    pub fn on_key(
        this: &Terminal,
        f: &Function, // Event<{key: &str, dom_event: KeyboardEvent}>
    ) -> Disposable;
}

#[derive(Debug)]
pub struct TerminalBuffer {
    buf: Vec<u8>,
    terminal: Terminal,
}

impl TerminalBuffer {
    pub fn new(terminal: Terminal) -> Self {
        const DEFAULT_CAPACITY: usize = 1024;

        TerminalBuffer {
            buf: Vec::with_capacity(DEFAULT_CAPACITY),
            terminal,
        }
    }
}

impl io::Write for TerminalBuffer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }

        // Note that while the `write` method for xtermjs accepts either bytes
        // or a string, they behave suitably different in that we can't just
        // pass the bytes directly.
        //
        // I don't know what exactly the issue is, since the docs say that bytes
        // are treated as utf8, and we're only ever generating valid utf8, but
        // we still end up with some weird rendering (missing newlines, lines
        // out of order, etc). Unfortunately it works "just enough" to make you
        // think it's a bug in your code, and you end up spending hours trying
        // to figure it out.
        //
        // String works, so whatever.
        //
        // `write` doc: https://xtermjs.org/docs/api/terminal/classes/terminal/#write
        let s = std::str::from_utf8(&self.buf)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        self.terminal.write(s);

        self.buf.clear();

        Ok(())
    }
}

/// Wrapper around a database session and the engine that created it.
///
/// This is expected to be instantiated on the javascript side.
#[wasm_bindgen]
#[derive(Debug)]
#[allow(dead_code)]
pub struct WasmShell {
    /// Shareable inner logic for the shell.
    pub(crate) inner: Rc<RefCell<ShellHandler>>,
}

#[wasm_bindgen]
impl WasmShell {
    /// Create a new shell that writes its output to the provided terminal.
    pub fn try_new(terminal: Terminal) -> Result<WasmShell> {
        let session = WasmSession::try_new()?;

        let terminal = TerminalBuffer::new(terminal);
        let mut shell = InteractiveShell::start(
            terminal,
            NopRawMode,
            None,
            Shell::new(session.engine),
            "GlareDB Wasm Shell",
        )?;

        let edit_guard = shell.edit_start()?;

        let inner = ShellHandler {
            shell,
            edit_guard: Some(edit_guard),
        };

        Ok(WasmShell {
            inner: Rc::new(RefCell::new(inner)),
        })
    }

    /// Notify on terminal resize.
    pub fn on_resize(&self, cols: usize) {
        trace!(%cols, "setting columns");
        if let Ok(mut inner) = self.inner.try_borrow_mut() {
            inner.shell.set_size(TermSize { cols });
        }
    }

    /// Consume a string verbatim.
    pub fn on_data(&self, text: String) -> Result<()> {
        trace!(%text, "consuming data");
        // TODO: Not sure what we want to do yet if this errors.
        if let Ok(mut inner) = self.inner.try_borrow_mut() {
            inner.shell.consume_text(&text)?;
        }
        Ok(())
    }

    /// Consume a keyboard event.
    ///
    /// This should be hooked up to xterm's `attachCustomKeyEventHandler`. This
    /// will handle _all_ input, so `false` should be returned to xterm after
    /// calling this for an event.
    pub fn on_key(&self, event: KeyboardEvent) -> Result<()> {
        ShellHandler::on_key(self.inner.clone(), event)
    }
}

#[derive(Debug)]
pub(crate) struct ShellHandler {
    shell: InteractiveShell<TerminalBuffer, WasmExecutor, WasmSystemRuntime, NopRawMode>,
    edit_guard: Option<WasmEditGuard>,
}

type WasmEditGuard = RawModeGuard<NopRawMode>;

/// Implementation of raw mode term that does nothing.
///
/// xterm.js is essentially always in raw mode, there no notion of "cooked" mode
/// for it, so there's no state for use to return to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NopRawMode;

impl RawModeTerm for NopRawMode {
    fn enable_raw_mode(&self) {}
    fn disable_raw_mode(&self) {}
}

#[derive(Debug)]
enum ShellHandlerInput {
    Key(KeyEvent),
    Text(String),
}

impl ShellHandler {
    fn on_key(handler: Rc<RefCell<Self>>, event: KeyboardEvent) -> Result<()> {
        if event.type_() != "keydown" {
            return Ok(());
        }

        let key = event.key();

        let key = match key.as_str() {
            "Backspace" => KeyEvent::Backspace,
            "Enter" => KeyEvent::Enter,
            "ArrowLeft" => KeyEvent::Left,
            "ArrowRight" => KeyEvent::Right,
            "ArrowUp" => KeyEvent::Up,
            "ArrowDown" => KeyEvent::Down,
            other if other.chars().count() != 1 => {
                warn!(%other, "unhandled input");
                return Ok(());
            }
            other => match other.chars().next() {
                Some(ch) => {
                    match ch {
                        'v' if event.meta_key() || event.ctrl_key() => {
                            // Handle both 'ctrl-v' and 'cmd-v' as paste to
                            // avoid needing to try to detect os.
                            ShellHandler::handle_paste(handler);
                            return Ok(());
                        }
                        'c' if event.ctrl_key() => {
                            // TODO: Not sure if this fine since it'll prevent
                            // non-mac users from copying using a shortcut.
                            KeyEvent::CtrlC
                        }
                        _ch if event.meta_key() => {
                            warn!(%other, "unhandled input with meta modifier");
                            return Ok(());
                        }
                        _ch if event.ctrl_key() => {
                            warn!(%other, "unhandled input with ctrl modifier");
                            return Ok(());
                        }
                        ch => KeyEvent::Char(ch),
                    }
                }
                None => {
                    warn!("key event with no key");
                    return Ok(());
                }
            },
        };

        ShellHandler::handle_input(handler, ShellHandlerInput::Key(key))
    }

    /// Handles paste by reading from the window's clipboard.
    fn handle_paste(handler: Rc<RefCell<Self>>) {
        async fn inner(handler: Rc<RefCell<ShellHandler>>) -> Result<()> {
            let window = window().required("Global window object")?;
            let clipboard = window.navigator().clipboard();
            let js_val = JsFuture::from(clipboard.read_text())
                .await
                .map_err(|_| DbError::new("Failed to read from the clipboard"))?;
            let text = js_val.as_string().unwrap();
            ShellHandler::handle_input(handler, ShellHandlerInput::Text(text))
        }

        spawn_local(async move {
            if let Err(e) = inner(handler).await {
                warn!(%e, "failed to handle paste");
            }
        });
    }

    fn handle_input(handler: Rc<RefCell<Self>>, input: ShellHandlerInput) -> Result<()> {
        let mut inner = match handler.try_borrow_mut() {
            Ok(inner) => inner,
            Err(_) => {
                // TODO: Not sure what to do yet.
                error!("multiple mutable borrws inner shell");
                return Ok(());
            }
        };

        match input {
            ShellHandlerInput::Text(text) => {
                // TODO: This should take the guard possibly.
                inner.shell.consume_text(&text)?;
                Ok(())
            }
            ShellHandlerInput::Key(key) => {
                let guard = match inner.edit_guard.take() {
                    Some(guard) => guard,
                    None => {
                        // TODO: ???
                        warn!("missing post execute guard");
                        return Ok(());
                    }
                };

                match inner.shell.consume_key(guard, key)? {
                    ShellSignal::Continue(guard) => {
                        inner.edit_guard = Some(guard);
                        // Continue with normal editing.
                    }
                    ShellSignal::ExecutePending(guard) => {
                        // Clone handler, we'll get a new mutable ref in the spawn.
                        //
                        // Needed since the spawn closure needs to be static.
                        let handler = handler.clone();

                        spawn_local(
                            // Holding the ref through the await here is fine, it
                            // essentially acts as a lock preventing additional input to
                            // the shell.
                            //
                            // TODO: We will want a way to cancel a running query
                            // though. That'll probably happen through a separate
                            // rc+refcell bool.
                            #[allow(clippy::await_holding_refcell_ref)]
                            async move {
                                let mut inner = match handler.try_borrow_mut() {
                                    Ok(inner) => inner,
                                    Err(e) => {
                                        error!(%e, "error getting mut ref to inner for query execute");
                                        return;
                                    }
                                };

                                if let Err(e) = inner.shell.execute_pending(guard).await {
                                    error!(%e, "error executing pending query");
                                }

                                match inner.shell.edit_start() {
                                    Ok(guard) => inner.edit_guard = Some(guard),
                                    Err(e) => {
                                        // Nothing we can really do.
                                        error!(%e, "error calling edit start for shell");
                                    }
                                }
                            },
                        );
                    }
                    ShellSignal::Exit => {
                        // Can't exit out of the web shell.
                        inner.edit_guard = Some(inner.shell.edit_start()?);
                    }
                }

                Ok(())
            }
        }
    }
}
