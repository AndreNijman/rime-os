import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.serialization)
}

kotlin {
    // Built by a 21 toolchain, emitting 17 bytecode.
    //
    // The two numbers are different on purpose. 21 is what this machine and CI
    // actually have; 17 is what the Android plugin compiles the app against,
    // and a `:core` emitting class file 65 would link here and fail at
    // `assembleDebug` — the slowest possible way to find out that a pure-JVM
    // module is consumed by an Android one.
    jvmToolchain(21)
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_17)
        // A warning in a protocol implementation is a place where the compiler
        // knows something the author did not.
        allWarningsAsErrors.set(true)
    }
}

java {
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
}

dependencies {
    implementation(libs.bouncycastle.provider)
    // `api`: the nex client hands JsonElement across its public surface.
    api(libs.kotlinx.serialization.json)
    testImplementation(libs.junit.jupiter)
    testRuntimeOnly(libs.junit.platform.launcher)
}

tasks.test {
    useJUnitPlatform()
    testLogging {
        events("failed")
        exceptionFormat = org.gradle.api.tasks.testing.logging.TestExceptionFormat.FULL
    }
}
