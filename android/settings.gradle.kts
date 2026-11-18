// The Android client for Rime Remote.
//
// It lives inside rime-os rather than in a repository of its own because
// `queue.json`'s `rime-android` does not exist and creating it would mean a
// new `main`, which the roadmap program forbids until final integration.
// Splitting it out later costs one `git filter-repo`; splitting it out now
// costs a rule.

pluginManagement {
    repositories {
        google {
            content {
                includeGroupByRegex("com\\.android.*")
                includeGroupByRegex("com\\.google.*")
                includeGroupByRegex("androidx.*")
            }
        }
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "rime-remote-android"

// `core` is deliberately a plain Kotlin/JVM module and not an Android library.
// The protocol, the framing and the handshake have nothing Android in them,
// and keeping them out of the Android plugin means they are testable on a JVM
// with no SDK, no device and no emulator — which is the only way they can be
// tested at all under this project's headless rule.
include(":core")
include(":app")

// The device suite's stand-in messaging app (debug only; see its build file).
include(":testhelper")
