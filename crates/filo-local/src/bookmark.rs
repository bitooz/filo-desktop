// macOS bookmarks (NSURL `bookmarkData`): a file's identity on its disk, kept beside its path, so a file that's renamed
// or moved on the same disk is found again. An agent's file sent with its message (mcp.rs `send_message`'s `files`)
// keeps one, and the chat finds it with it when its path no longer works (attachments.rs `moved_to`).

use std::path::{Path, PathBuf};

/// The bookmark for the file at `path`; none where it can't be made (or off macOS).
#[cfg(target_os = "macos")]
pub fn of(path: &Path) -> Option<Vec<u8>> {
    use objc2_foundation::{NSString, NSURL, NSURLBookmarkCreationOptions};
    objc2::rc::autoreleasepool(|_| {
        let url = NSURL::fileURLWithPath(&NSString::from_str(path.to_str()?));
        let data = url
            .bookmarkDataWithOptions_includingResourceValuesForKeys_relativeToURL_error(
                NSURLBookmarkCreationOptions::empty(),
                None,
                None,
            )
            .ok()?;
        Some(data.to_vec())
    })
}

/// Where the file a bookmark (`of`) names is now, without asking the user or mounting a disk; none when it's gone.
#[cfg(target_os = "macos")]
pub fn resolve(bookmark: &[u8]) -> Option<PathBuf> {
    use objc2_foundation::{NSData, NSURL, NSURLBookmarkResolutionOptions};
    objc2::rc::autoreleasepool(|_| {
        let data = NSData::with_bytes(bookmark);
        let mut stale = objc2::runtime::Bool::NO;
        // SAFETY: `stale` is a valid pointer for the call.
        let url = unsafe {
            NSURL::URLByResolvingBookmarkData_options_relativeToURL_bookmarkDataIsStale_error(
                &data,
                NSURLBookmarkResolutionOptions::WithoutUI | NSURLBookmarkResolutionOptions::WithoutMounting,
                None,
                &mut stale,
            )
        }
        .ok()?;
        Some(PathBuf::from(url.path()?.to_string()))
    })
}

#[cfg(not(target_os = "macos"))]
pub fn of(_: &Path) -> Option<Vec<u8>> {
    None
}

#[cfg(not(target_os = "macos"))]
pub fn resolve(_: &[u8]) -> Option<PathBuf> {
    None
}
