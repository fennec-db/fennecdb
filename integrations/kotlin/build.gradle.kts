plugins {
    kotlin("jvm") version "2.0.21" apply false
    id("com.android.library") version "8.5.2" apply false
    kotlin("android") version "2.0.21" apply false
}

// One version wherever a release reads it (tools/version.py writes it).
allprojects {
    group = "com.fennecdb"
    version = "0.2.0"
}
