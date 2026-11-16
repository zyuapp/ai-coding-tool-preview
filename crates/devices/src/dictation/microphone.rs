//! Asking for the microphone in the app's own name. Only macOS asks; elsewhere it is simply there.

/// Whether the app may hear the microphone, asking the person the first time.
#[cfg(target_os = "macos")]
pub async fn microphone_access() -> bool {
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};

    // SAFETY: AVMediaTypeAudio is a constant AVFoundation exports for the process's lifetime.
    let Some(audio) = (unsafe { AVMediaTypeAudio }) else { return false };
    // SAFETY: the media type is audio, which this call accepts.
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(audio) };
    if status == AVAuthorizationStatus::Authorized {
        return true;
    }
    if status != AVAuthorizationStatus::NotDetermined {
        return false;
    }
    let (sender, receiver) = futures::channel::oneshot::channel::<bool>();
    {
        let sender = std::sync::Mutex::new(Some(sender));
        let block = RcBlock::new(move |granted: Bool| {
            if let Some(sender) = sender.lock().unwrap().take() {
                let _ = sender.send(granted.as_bool());
            }
        });
        // SAFETY: the handler block is copied by AVFoundation and runs once on a queue of its choosing.
        unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(audio, &block) };
    }
    receiver.await.unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
pub async fn microphone_access() -> bool {
    true
}
