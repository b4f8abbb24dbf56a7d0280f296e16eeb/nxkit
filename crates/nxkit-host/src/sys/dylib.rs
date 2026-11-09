//! The libraries that `dlopen` loads and the images that dyld reports.

use super::*;

/// A library that `dlopen` loaded. It is never unloaded, except by `close`.
#[derive(Debug)]
pub struct Dylib {
    handle: *mut c_void,
    path: PathBuf,
}

// SAFETY: a `dlopen` handle is a plain token that dyld accepts from any thread.
unsafe impl Send for Dylib {}
// SAFETY: as for `Send`.
unsafe impl Sync for Dylib {}

fn dl_message() -> String {
    // SAFETY: `dlerror` returns null or a C string that stays valid until the next dl call on this thread.
    let p = unsafe { dlerror() };
    if p.is_null() {
        return "unknown error".into();
    }
    // SAFETY: `p` is not null and points to a C string, see above.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

impl Dylib {
    /// Loads a library. The path is made absolute first, so that the image can be found by it afterwards.
    pub fn open(path: &Path) -> Result<Dylib> {
        let path = std::fs::canonicalize(path).map_err(|e| HostError::io(path, &e))?;
        let c = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|_| HostError::Load(format!("{} has a nul byte", path.display())))?;
        // SAFETY: `c` is a valid C string.
        let handle = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_LOCAL) };
        if handle.is_null() {
            return Err(HostError::Load(format!(
                "dlopen {}: {}",
                path.display(),
                dl_message()
            )));
        }
        Ok(Dylib { handle, path })
    }

    /// The Mach-O image that `dlopen` of this library mapped: its header address and its slide.
    ///
    /// The library has no symbols, so the image is found by its path among the images of dyld. The load commands are
    /// read from the mapped header.
    pub fn image(&self) -> Result<Image> {
        // SAFETY: `_dyld_image_count` has no preconditions.
        let count = unsafe { _dyld_image_count() };
        for i in 0..count {
            // SAFETY: `i` is below the count. The name is a C string that lives as long as the image.
            let name = unsafe { _dyld_get_image_name(i) };
            if name.is_null() {
                continue;
            }
            // SAFETY: `name` is not null and is a C string, see above.
            let name = unsafe { CStr::from_ptr(name) }.to_string_lossy();
            if std::fs::canonicalize(&*name).ok().as_deref() != Some(self.path.as_path()) {
                continue;
            }
            // SAFETY: `i` is below the count.
            let (head, slide) =
                unsafe { (_dyld_get_image_header(i), _dyld_get_image_vmaddr_slide(i)) };
            if head.is_null() {
                return Err(HostError::Load(
                    "dyld gave no header for the library".into(),
                ));
            }
            return Ok(Image {
                header: head as usize as u64,
                slide: slide as i64,
            });
        }
        Err(HostError::Load(format!(
            "dyld does not list {} as a loaded image",
            self.path.display()
        )))
    }

    /// Unloads the library.
    pub fn close(self) {
        // SAFETY: the handle is live and `self` is consumed.
        unsafe { dlclose(self.handle) };
    }
}

/// A Mach-O image that dyld mapped.
#[derive(Debug, Clone, Copy)]
pub struct Image {
    /// The address of the Mach-O header.
    pub header: u64,
    /// How far dyld moved the image from its preferred addresses.
    pub slide: i64,
}

impl Image {
    /// The header and the load commands, copied out of the mapping.
    pub fn commands(&self) -> Result<Vec<u8>> {
        let mut head = [0u8; macho::HEADER_SIZE];
        // SAFETY: dyld mapped the header at this address, it is readable for the life of the image.
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.header as usize as *const u8,
                head.as_mut_ptr(),
                head.len(),
            )
        };
        let total = macho::commands_end(&head)?;
        let mut all = vec![0u8; total];
        // SAFETY: the load commands directly follow the header in the same mapped segment.
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.header as usize as *const u8,
                all.as_mut_ptr(),
                total,
            )
        };
        Ok(all)
    }
}
