//! Handing a downloaded APK to Android's package installer.
//!
//! The update used to open the APK's URL in a browser, which meant leaving
//! the app, a download in the notification shade, and a tap to find it again.
//! Downloading inside the app is easy; launching the installer is not, and no
//! Tauri plugin covers it:
//!
//! - A `file://` URI is refused outright since Android 7 (FileUriExposed).
//! - A `content://` URI needs `FLAG_GRANT_READ_URI_PERMISSION`, or the
//!   installer cannot read what it is told to install. The opener plugin sets
//!   only `FLAG_ACTIVITY_NEW_TASK` and no MIME type, so it cannot be used.
//!
//! So this builds the Intent itself over JNI. The FileProvider it needs
//! already exists — Tauri's own Android library declares the authority
//! `<package>.fileprovider` over `<cache-path path="."/>` — which is why the
//! download lands in the cache directory rather than anywhere tidier.
//!
//! Android still shows its own install prompt. That is not avoidable for a
//! sideloaded app and is not worth trying to avoid: the confirmation is the
//! user's, not ours.

use std::path::Path;

#[cfg(target_os = "android")]
pub fn install_apk(path: &Path) -> Result<(), String> {
    use jni::objects::{JObject, JValue};

    let ctx = ndk_context::android_context();
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }
        .map_err(|e| format!("no JVM: {e}"))?;
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
    let mut env = vm
        .attach_current_thread()
        .map_err(|e| format!("could not attach to the JVM: {e}"))?;

    let path_str = path.to_string_lossy().to_string();

    let jpath = env.new_string(&path_str).map_err(|e| e.to_string())?;
    let file = env
        .new_object("java/io/File", "(Ljava/lang/String;)V", &[JValue::Object(&jpath.into())])
        .map_err(|e| format!("File: {e}"))?;

    let package = env
        .call_method(&activity, "getPackageName", "()Ljava/lang/String;", &[])
        .and_then(|v| v.l())
        .map_err(|e| format!("getPackageName: {e}"))?;
    let package: String = env
        .get_string((&package).into())
        .map_err(|e| e.to_string())?
        .into();
    let authority = env
        .new_string(format!("{package}.fileprovider"))
        .map_err(|e| e.to_string())?;

    let uri = env
        .call_static_method(
            "androidx/core/content/FileProvider",
            "getUriForFile",
            "(Landroid/content/Context;Ljava/lang/String;Ljava/io/File;)Landroid/net/Uri;",
            &[
                JValue::Object(&activity),
                JValue::Object(&authority.into()),
                JValue::Object(&file),
            ],
        )
        .and_then(|v| v.l())
        .map_err(|e| format!("getUriForFile ({path_str}): {e}"))?;

    let action = env.new_string("android.intent.action.VIEW").map_err(|e| e.to_string())?;
    let intent = env
        .new_object(
            "android/content/Intent",
            "(Ljava/lang/String;)V",
            &[JValue::Object(&action.into())],
        )
        .map_err(|e| format!("Intent: {e}"))?;

    let mime = env
        .new_string("application/vnd.android.package-archive")
        .map_err(|e| e.to_string())?;
    env.call_method(
        &intent,
        "setDataAndType",
        "(Landroid/net/Uri;Ljava/lang/String;)Landroid/content/Intent;",
        &[JValue::Object(&uri), JValue::Object(&mime.into())],
    )
    .map_err(|e| format!("setDataAndType: {e}"))?;

    env.call_method(
        &intent,
        "addFlags",
        "(I)Landroid/content/Intent;",
        &[JValue::Int(0x1000_0001)],
    )
    .map_err(|e| format!("addFlags: {e}"))?;

    env.call_method(
        &activity,
        "startActivity",
        "(Landroid/content/Intent;)V",
        &[JValue::Object(&intent)],
    )
    .map_err(|e| format!("startActivity: {e}"))?;

    Ok(())
}

/// On desktop there is no package installer; hand the file to the OS.
#[cfg(not(target_os = "android"))]
pub fn install_apk(path: &Path) -> Result<(), String> {
    let _ = path;
    Err("installing an APK only applies on Android".into())
}
