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
//! class loader. And a Java exception must not be left pending: it once
//! killed the app. jni clears any it finds when the attachment closure
//! returns, but it would also replace our error, which names the step that
//! failed, so each one is cleared and described here first.
//!
//! Android still shows its own install prompt. That is not avoidable for a
//! sideloaded app and is not worth trying to avoid: the confirmation is the
//! user's, not ours.

use std::path::Path;

#[cfg(target_os = "android")]
pub fn install_apk(path: &Path) -> Result<(), String> {
    use jni::objects::JObject;

    let ctx = ndk_context::android_context();
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) };

    // The outer Result is jni's own (attaching, or an exception jni had to
    // catch); the inner one is ours, naming the step that failed.
    let attached = vm.attach_current_thread(|env| -> jni::errors::Result<Result<(), String>> {
        // ndk_context holds the activity for the app's lifetime; this only
        // borrows that reference, and JObject never deletes it.
        let activity = unsafe { JObject::from_raw(env, ctx.context().cast()) };
        let result = start_installer(env, &activity, path);
        if env.exception_check() {
            let thrown = env.exception_occurred();
            env.exception_clear();
            let described = thrown.and_then(|t| describe(env, &t));
            if let (Err(step), Some(java)) = (&result, described) {
                return Ok(Err(format!("{step}: {java}")));
            }
        }
        Ok(result)
    });
    attached.unwrap_or_else(|e| Err(format!("JNI: {e}")))
}

#[cfg(target_os = "android")]
fn describe(env: &mut jni::Env, thrown: &jni::objects::JThrowable) -> Option<String> {
    use jni::{jni_sig, jni_str, objects::JString};

    let text = env.call_method(
        thrown,
        jni_str!("toString"),
        jni_sig!("()Ljava/lang/String;"),
        &[],
    );
    // toString can throw too; never leave that pending either.
    env.exception_clear();
    let text = env.cast_local::<JString>(text.ok()?.l().ok()?).ok()?;
    text.try_to_string(env).ok()
}

#[cfg(target_os = "android")]
fn start_installer(
    env: &mut jni::Env,
    activity: &jni::objects::JObject,
    path: &Path,
) -> Result<(), String> {
    use jni::{
        jni_sig, jni_str,
        objects::{JClass, JString, JValue},
    };

    let path_str = path.to_string_lossy().to_string();

    let jpath = env.new_string(&path_str).map_err(|e| e.to_string())?;
    let file = env
        .new_object(
            jni_str!("java/io/File"),
            jni_sig!("(Ljava/lang/String;)V"),
            &[JValue::Object(&jpath)],
        )
        .map_err(|e| format!("File: {e}"))?;

    let package = env
        .call_method(
            activity,
            jni_str!("getPackageName"),
            jni_sig!("()Ljava/lang/String;"),
            &[],
        )
        .and_then(|v| v.l())
        .and_then(|o| env.cast_local::<JString>(o))
        .and_then(|s| s.try_to_string(env))
        .map_err(|e| format!("getPackageName: {e}"))?;
    let authority = env
        .new_string(format!("{package}.fileprovider"))
        .map_err(|e| e.to_string())?;

    let loader = env
        .call_method(
            activity,
            jni_str!("getClassLoader"),
            jni_sig!("()Ljava/lang/ClassLoader;"),
            &[],
        )
        .and_then(|v| v.l())
        .map_err(|e| format!("getClassLoader: {e}"))?;
    let class_name = env
        .new_string("androidx.core.content.FileProvider")
        .map_err(|e| e.to_string())?;
    let provider = env
        .call_method(
            &loader,
            jni_str!("loadClass"),
            jni_sig!("(Ljava/lang/String;)Ljava/lang/Class;"),
            &[JValue::Object(&class_name)],
        )
        .and_then(|v| v.l())
        .and_then(|o| env.cast_local::<JClass>(o))
        .map_err(|e| format!("loading FileProvider: {e}"))?;

    let uri = env
        .call_static_method(
            &provider,
            jni_str!("getUriForFile"),
            jni_sig!(
                "(Landroid/content/Context;Ljava/lang/String;Ljava/io/File;)Landroid/net/Uri;"
            ),
            &[
                JValue::Object(activity),
                JValue::Object(&authority),
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
            jni_str!("android/content/Intent"),
            jni_sig!("(Ljava/lang/String;)V"),
            &[JValue::Object(&action)],
        )
        .map_err(|e| format!("Intent: {e}"))?;

    let mime = env
        .new_string("application/vnd.android.package-archive")
        .map_err(|e| e.to_string())?;
    env.call_method(
        &intent,
        jni_str!("setDataAndType"),
        jni_sig!("(Landroid/net/Uri;Ljava/lang/String;)Landroid/content/Intent;"),
        &[JValue::Object(&uri), JValue::Object(&mime)],
    )
    .map_err(|e| format!("setDataAndType: {e}"))?;

    const FLAG_GRANT_READ_URI_PERMISSION: i32 = 0x0000_0001;
    const FLAG_ACTIVITY_NEW_TASK: i32 = 0x1000_0000;
    env.call_method(
        &intent,
        jni_str!("addFlags"),
        jni_sig!("(I)Landroid/content/Intent;"),
        &[JValue::Int(
            FLAG_GRANT_READ_URI_PERMISSION | FLAG_ACTIVITY_NEW_TASK,
        )],
    )
    .map_err(|e| format!("addFlags: {e}"))?;

    env.call_method(
        activity,
        jni_str!("startActivity"),
        jni_sig!("(Landroid/content/Intent;)V"),
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
