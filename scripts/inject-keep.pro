# R8 rules for the release APK. build-apk.sh copies this into the generated
# Android project, whose release build reads every *.pro file there.
#
# The in-app updater (app/src-tauri/src/install.rs) calls this through JNI, by
# name. R8 cannot see that call, so it renamed every FileProvider method to a
# single letter and the update failed with NoSuchMethodError. The class itself
# survives because the manifest names it; its methods need this rule.
-keep class androidx.core.content.FileProvider {
    public static android.net.Uri getUriForFile(android.content.Context, java.lang.String, java.io.File);
}
