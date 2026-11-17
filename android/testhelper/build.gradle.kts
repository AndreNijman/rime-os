// A stand-in messaging app for the device suite, and nothing else.
//
// Rime Remote never mirrors its own notifications, and the shell's
// `cmd notification post` has no reply action, so the reply test needs a
// third app whose notification carries a free-form RemoteInput.
// `android/tools/run-device-suite.sh` builds and installs it. Debug only: it
// has no release build, so no release workflow can pick it up by accident.
plugins {
    alias(libs.plugins.android.application)
}

android {
    namespace = "com.rimeos.remote.testhelper"
    compileSdk = 36

    defaultConfig {
        applicationId = "com.rimeos.remote.testhelper"
        minSdk = 28
        targetSdk = 36
        versionCode = 1
        versionName = "test"
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

androidComponents {
    beforeVariants(selector().withBuildType("release")) { it.enable = false }
}

kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
}
