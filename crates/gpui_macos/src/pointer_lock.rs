use core_graphics::display::CGDisplay;
use std::{
    marker::PhantomData,
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
};

static POINTER_LOCKED: AtomicBool = AtomicBool::new(false);

pub(crate) struct PointerLock {
    display: CGDisplay,
    _main_thread: PhantomData<Rc<()>>,
}

impl PointerLock {
    pub(crate) fn acquire() -> anyhow::Result<Self> {
        anyhow::ensure!(
            !POINTER_LOCKED.swap(true, Ordering::AcqRel),
            "another window owns the pointer lock"
        );
        let display = CGDisplay::main();
        if let Err(error) = CGDisplay::associate_mouse_and_mouse_cursor_position(false) {
            POINTER_LOCKED.store(false, Ordering::Release);
            anyhow::bail!("cannot detach mouse cursor: {error:?}");
        }
        if let Err(error) = display.hide_cursor() {
            if let Err(restore_error) = CGDisplay::associate_mouse_and_mouse_cursor_position(true) {
                log::error!("cannot restore mouse cursor association: {restore_error:?}");
            }
            POINTER_LOCKED.store(false, Ordering::Release);
            anyhow::bail!("cannot hide mouse cursor: {error:?}");
        }
        Ok(Self {
            display,
            _main_thread: PhantomData,
        })
    }
}

impl Drop for PointerLock {
    fn drop(&mut self) {
        if let Err(error) = CGDisplay::associate_mouse_and_mouse_cursor_position(true) {
            log::error!("cannot restore mouse cursor association: {error:?}");
        }
        if let Err(error) = self.display.show_cursor() {
            log::error!("cannot restore mouse cursor visibility: {error:?}");
        }
        POINTER_LOCKED.store(false, Ordering::Release);
    }
}
