plugins {
    id("com.android.application")
}

// Release builds sign with the keystore named by ANDROID_KEYSTORE, the way
// CI does, so updates install over each other; otherwise the debug key.
// The key uses the keystore's password.
val keystore: String? = System.getenv("ANDROID_KEYSTORE")?.takeIf { it.isNotBlank() }

android {
    namespace = "com.baseball.tracker"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.baseball.tracker"
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
    }

    signingConfigs {
        if (keystore != null) {
            create("release") {
                storeFile = file(keystore)
                storePassword = System.getenv("ANDROID_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("ANDROID_KEY_ALIAS")
                keyPassword = storePassword
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName(if (keystore != null) "release" else "debug")
        }
    }
}

dependencies {
    implementation("androidx.activity:activity:1.10.1")
}
