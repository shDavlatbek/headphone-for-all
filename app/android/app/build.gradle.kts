import java.util.Properties

plugins {
    id("com.android.application")
    // The Flutter Gradle Plugin must be applied after the Android and Kotlin Gradle plugins.
    id("dev.flutter.flutter-gradle-plugin")
}

// Release signing key (docs/BUILDING.md, "Releasing"): android/key.properties (storeFile relative
// to android/app, storePassword, keyAlias, keyPassword; git-ignored), else the environment
// variables HFA_ANDROID_KEYSTORE (path), HFA_ANDROID_KEYSTORE_PASSWORD, HFA_ANDROID_KEY_ALIAS and
// HFA_ANDROID_KEY_PASSWORD (defaults to the store password), which CI fills from secrets. Without
// a key, release builds are signed with the debug key (installable for testing, not for a store)
// and Gradle warns; HFA_ANDROID_REQUIRE_RELEASE_KEY=true turns that into an error.
class ReleaseKey(val storeFile: File, val storePassword: String, val keyAlias: String, val keyPassword: String)

val releaseKey: ReleaseKey? = run {
    val properties = Properties()
    val propertiesFile = rootProject.file("key.properties")
    if (propertiesFile.isFile) {
        propertiesFile.inputStream().use { properties.load(it) }
    }
    fun setting(property: String, variable: String): String? =
        (properties.getProperty(property) ?: System.getenv(variable))?.takeIf { it.isNotBlank() }

    val storePath = setting("storeFile", "HFA_ANDROID_KEYSTORE") ?: return@run null
    val storePassword = setting("storePassword", "HFA_ANDROID_KEYSTORE_PASSWORD")
    val keyAlias = setting("keyAlias", "HFA_ANDROID_KEY_ALIAS")
    if (storePassword == null || keyAlias == null) {
        throw GradleException(
            "Release signing: a keystore is configured but its password or key alias is missing " +
                "(key.properties or HFA_ANDROID_KEYSTORE_PASSWORD / HFA_ANDROID_KEY_ALIAS)."
        )
    }
    val storeFile = file(storePath)
    if (!storeFile.isFile) {
        throw GradleException("Release signing: keystore $storeFile does not exist.")
    }
    ReleaseKey(
        storeFile,
        storePassword,
        keyAlias,
        setting("keyPassword", "HFA_ANDROID_KEY_PASSWORD") ?: storePassword,
    )
}

if (releaseKey == null) {
    gradle.taskGraph.whenReady {
        if (allTasks.any { it.project == project && it.name.contains("Release") }) {
            if (System.getenv("HFA_ANDROID_REQUIRE_RELEASE_KEY") == "true") {
                throw GradleException(
                    "HFA_ANDROID_REQUIRE_RELEASE_KEY is set but no release key is configured."
                )
            }
            logger.warn(
                "warning: no release signing key (key.properties or HFA_ANDROID_KEYSTORE); " +
                    "the release build is signed with the debug key."
            )
        }
    }
}

android {
    namespace = "io.github.shdavlatbek.hfa"
    compileSdk = flutter.compileSdkVersion
    // cargokit (app/rust_builder) builds the Rust core (core/hfa-ffi) with this NDK.
    ndkVersion = "29.0.14206865"

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    defaultConfig {
        applicationId = "io.github.shdavlatbek.hfa"
        // Android 10: AudioPlaybackCapture (system audio capture) needs API 29.
        minSdk = 29
        targetSdk = flutter.targetSdkVersion
        // Uses the version code from pubspec.yaml. When using split APKs, 1000 * ABI_VERSION
        // is added automatically by Flutter. (https://developer.android.com/studio/build/configure-apk-splits#configure-APK-versions)
        // You can force using the value of versionCode by specifying the `-P force-version-code-ignoring-abi=true`
        // flag during build.
        versionCode = flutter.versionCode
        versionName = flutter.versionName
    }

    signingConfigs {
        if (releaseKey != null) {
            create("release") {
                storeFile = releaseKey.storeFile
                storePassword = releaseKey.storePassword
                keyAlias = releaseKey.keyAlias
                keyPassword = releaseKey.keyPassword
            }
        }
    }

    buildTypes {
        release {
            // The release key when one is configured (see releaseKey above), else the debug key,
            // so `flutter run --release` and CI test builds keep working.
            signingConfig = signingConfigs.getByName(if (releaseKey != null) "release" else "debug")
        }
    }
}

kotlin {
    compilerOptions {
        jvmTarget = org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17
    }
}

flutter {
    source = "../.."
}

dependencies {
    // JVM unit tests of the pure capture logic (src/test).
    testImplementation("junit:junit:4.13.2")
}
