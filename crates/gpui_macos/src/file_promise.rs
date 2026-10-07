use block2::DynBlock;
use gpui::PromisedFileDescriptor;
use objc2::{
    AnyThread, DefinedClass, define_class, msg_send, rc::Retained, runtime::ProtocolObject,
};
use objc2_app_kit::{NSFilePromiseProvider, NSFilePromiseProviderDelegate};
use objc2_foundation::{NSError, NSObject, NSObjectProtocol, NSOperationQueue, NSString, NSURL};
use std::{
    ffi::{CStr, OsStr},
    os::unix::ffi::OsStrExt,
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
};

struct PromiseIvars {
    file: PromisedFileDescriptor,
    queue: Retained<NSOperationQueue>,
}
impl Drop for PromiseIvars {
    fn drop(&mut self) {
        self.file.provider.cancel();
    }
}
define_class!(
    // SAFETY: NSObject has no subclassing requirements; ivars own their Rust values.
    #[unsafe(super(NSObject))]
    #[ivars = PromiseIvars]
    struct FilePromiseDelegate;

    unsafe impl NSObjectProtocol for FilePromiseDelegate {}
    unsafe impl NSFilePromiseProviderDelegate for FilePromiseDelegate {
        #[unsafe(method_id(filePromiseProvider:fileNameForType:))]
        fn file_name(
            &self,
            _provider: &NSFilePromiseProvider,
            _file_type: &NSString,
        ) -> Retained<NSString> {
            NSString::from_str(&self.ivars().file.name.to_string_lossy())
        }
        #[unsafe(method_id(operationQueueForFilePromiseProvider:))]
        fn operation_queue(&self, _provider: &NSFilePromiseProvider) -> Retained<NSOperationQueue> {
            self.ivars().queue.clone()
        }
        #[unsafe(method(filePromiseProvider:writePromiseToURL:completionHandler:))]
        fn write_promise(
            &self,
            _provider: &NSFilePromiseProvider,
            url: &NSURL,
            completion: &DynBlock<dyn Fn(*mut NSError)>,
        ) {
            // AppKit calls this on the supplied operation queue. Its URL is the
            // complete destination, including the filename negotiated with Finder.
            let result = catch_unwind(AssertUnwindSafe(|| {
                if !url.isFileURL() {
                    return Err(std::io::ErrorKind::InvalidInput.into());
                }
                let bytes = unsafe { CStr::from_ptr(url.fileSystemRepresentation().as_ptr()) };
                self.ivars()
                    .file
                    .provider
                    .write_to(Path::new(OsStr::from_bytes(bytes.to_bytes())))
            }));
            if matches!(result, Ok(Ok(()))) {
                completion.call((std::ptr::null_mut(),));
            } else {
                // Do not expose remote paths, terminal data, or credentials in diagnostics.
                let error = unsafe {
                    NSError::errorWithDomain_code_userInfo(
                        &NSString::from_str("GPUIFilePromise"),
                        1,
                        None,
                    )
                };
                completion.call((Retained::as_ptr(&error).cast_mut(),));
            }
        }
    }
);

pub(crate) fn provider(file: PromisedFileDescriptor) -> Retained<NSFilePromiseProvider> {
    let file_type = NSString::from_str(if file.is_directory {
        "public.folder"
    } else {
        "public.data"
    });
    let queue = NSOperationQueue::new();
    queue.setMaxConcurrentOperationCount(1);
    let delegate = FilePromiseDelegate::alloc().set_ivars(PromiseIvars { file, queue });
    let delegate: Retained<FilePromiseDelegate> = unsafe { msg_send![super(delegate), init] };
    let provider = NSFilePromiseProvider::initWithFileType_delegate(
        NSFilePromiseProvider::alloc(),
        &file_type,
        ProtocolObject::from_ref(&*delegate),
    );
    // The delegate property is weak. userInfo retains it for precisely the
    // promise provider's lifetime, without retaining an application entity.
    unsafe {
        provider.setUserInfo(Some(&delegate));
    }
    provider
}
