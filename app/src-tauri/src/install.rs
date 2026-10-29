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
//! So this builds the Intent itself over JNI, through the FileProvider that
//! `scripts/inject-manifest.py` declares as `<package>.fileprovider` over the
//! cache's `updates/` folder, which is where the download lands.
//!
//! Two things about the thread this runs on. It is one Rust attached to the
//! JVM, and on such a thread `FindClass` searches only the system's classes,
//! so the app's own `FileProvider` has to be loaded through the activity's
//! class loader. And a Java exception left pending when the thread goes back
//! kills the app, so each one is cleared and returned as an error instead.
//!
//! Android still shows its own install prompt. That is not avoidable for a
//! sideloaded app and is not worth trying to avoid: the confirmation is the
//! user's, not ours.

use std::path::Path;

#[cfg(target_os = "android")]
pub fn install_apk(path: &Path) -> Result<(), String> {
    use jni::objects::JObject;

    let ctx = ndk_context::android_context();
    let vm =
        unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| format!("no JVM: {e}"))?;
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
    let mut env = vm
        .attach_current_thread()
        .map_err(|e| format!("could not attach to the JVM: {e}"))?;

    let result = start_installer(&mut env, &activity, path);
    if env.exception_check().unwrap_or(false) {
        let thrown = env.exception_occurred().ok();
        let _ = env.exception_clear();
        let described = thrown.and_then(|t| describe(&mut env, &t));
        if let (Err(step), Some(java)) = (&result, described) {
            return Err(format!("{step}: {java}"));
        }
    }
    result
}

#[cfg(target_os = "android")]
fn describe(env: &mut jni::JNIEnv, thrown: &jni::objects::JThrowable) -> Option<String> {
    let text = env
        .call_method(thrown, "toString", "()Ljava/lang/String;", &[])
        .and_then(|v| v.l())
        .ok()?;
    let _ = env.exception_clear();
    env.get_string((&text).into()).ok().map(Into::into)
}

#[cfg(target_os = "android")]
fn start_installer(
    env: &mut jni::JNIEnv,
    activity: &jni::objects::JObject,
    path: &Path,
) -> Result<(), String> {
    use jni::objects::{JClass, JValue};

    let path_str = path.to_string_lossy().to_string();

    let jpath = env.new_string(&path_str).map_err(|e| e.to_string())?;
    let file = env
        .new_object(
            "java/io/File",
            "(Ljava/lang/String;)V",
            &[JValue::Object(&jpath.into())],
        )
        .map_err(|e| format!("File: {e}"))?;

    let package = env
        .call_method(activity, "getPackageName", "()Ljava/lang/String;", &[])
        .and_then(|v| v.l())
        .map_err(|e| format!("getPackageName: {e}"))?;
    let package: String = env
        .get_string((&package).into())
        .map_err(|e| e.to_string())?
        .into();
    let authority = env
        .new_string(format!("{package}.fileprovider"))
        .map_err(|e| e.to_string())?;

    let loader = env
        .call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
        .and_then(|v| v.l())
        .map_err(|e| format!("getClassLoader: {e}"))?;
    let class_name = env
        .new_string("androidx.core.content.FileProvider")
        .map_err(|e| e.to_string())?;
    let provider = env
        .call_method(
            &loader,
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[JValue::Object(&class_name.into())],
        )
        .and_then(|v| v.l())
        .map_err(|e| format!("loading FileProvider: {e}"))?;
    let provider = JClass::from(provider);

    let uri = env
        .call_static_method(
            &provider,
            "getUriForFile",
            "(Landroid/content/Context;Ljava/lang/String;Ljava/io/File;)Landroid/net/Uri;",
            &[
                JValue::Object(activity),
                JValue::Object(&authority.into()),
                JValue::Object(&file),
            ],
        )
        .and_then(|v| v.l())
        .map_err(|e| format!("getUriForFile ({path_str}): {e}"))?;

    let action = env
        .new_string("android.intent.action.VIEW")
        .map_err(|e| e.to_string())?;
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

    const FLAG_GRANT_READ_URI_PERMISSION: i32 = 0x0000_0001;
    const FLAG_ACTIVITY_NEW_TASK: i32 = 0x1000_0000;
    env.call_method(
        &intent,
        "addFlags",
        "(I)Landroid/content/Intent;",
        &[JValue::Int(
            FLAG_GRANT_READ_URI_PERMISSION | FLAG_ACTIVITY_NEW_TASK,
        )],
    )
    .map_err(|e| format!("addFlags: {e}"))?;

    env.call_method(
        activity,
        "startActivity",
        "(Landroid/content/Intent;)V",
        &[JValue::Object(&intent)],
    )
    .map_err(|e| format!("startActivity: {e}"))?;

    Ok(())
}

/// On desktop there is no package installer.
#[cfg(not(target_os = "android"))]
pub fn install_apk(path: &Path) -> Result<(), String> {
    let _ = path;
    Err("installing an APK only applies on Android".into())
}
