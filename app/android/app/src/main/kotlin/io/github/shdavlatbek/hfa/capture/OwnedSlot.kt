package io.github.shdavlatbek.hfa.capture

/**
 * A single value together with the owner that set it: only that owner can clear it.
 *
 * [io.github.shdavlatbek.hfa.PlatformEvents] keeps the one process-wide event sink in it, keyed
 * by the Flutter engine (its `BinaryMessenger`) that listens. A new engine's `onListen` replaces
 * the sink; a late `onCancel` or cleanup of an older engine then leaves the new sink alone.
 *
 * Not thread-safe: the Android side uses it on the main thread only.
 */
class OwnedSlot<T : Any> {
    private var owner: Any? = null

    /** The current value, or `null`. */
    var value: T? = null
        private set

    /** [owner] now holds [value] (replacing any other owner). */
    fun set(owner: Any, value: T) {
        this.owner = owner
        this.value = value
    }

    /** Clears the value if [owner] holds it; returns whether it did. */
    fun clear(owner: Any): Boolean {
        if (this.owner !== owner) return false
        this.owner = null
        value = null
        return true
    }
}
