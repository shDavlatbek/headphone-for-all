package io.github.shdavlatbek.hfa.capture

/** Return codes of `NativeBridge.pushPcm` / `pushPcm16` (`HFA_*` in `core/hfa-ffi`). */
object HfaCode {
    /** Accepted (also while the sender is still connecting). */
    const val OK: Int = 0

    /** Invalid argument: the format differs from the registered feed, or the sizes are wrong. */
    const val INVALID_ARGUMENT: Int = -1

    /** JNI failure inside the native library. */
    const val ENGINE: Int = -3

    /** The feed id is not registered (before `sender_start` or after `sender_stop`). */
    const val UNKNOWN_FEED: Int = -4

    /** A panic was caught inside the native library. */
    const val INTERNAL: Int = -5
}

/**
 * Decides what the capture loop does with each push result.
 *
 * - [HfaCode.OK] resets both counters.
 * - [HfaCode.UNKNOWN_FEED] drops the audio: no sender reads the feed right now. Dart registers
 *   the feed (`sender_start`) before it starts the capture, so the feed is only missing after
 *   the sender was stopped (`sender_stop` unregisters it). [maxUnknownFeed] of them in a row end
 *   the capture with [LoopExit.NoSender]: a capture nobody reads (for example after the Flutter
 *   UI was recreated and lost track of it) must not keep the MediaProjection alive.
 * - [HfaCode.INVALID_ARGUMENT] fails at once: the format was rejected and will not change.
 * - Any other code counts as a transient error; [maxConsecutiveErrors] of them in a row fail.
 */
class PushPolicy(
    private val maxConsecutiveErrors: Int = DEFAULT_MAX_CONSECUTIVE_ERRORS,
    private val maxUnknownFeed: Int = DEFAULT_MAX_UNKNOWN_FEED,
) {
    init {
        require(maxConsecutiveErrors > 0) { "maxConsecutiveErrors must be positive" }
        require(maxUnknownFeed > 0) { "maxUnknownFeed must be positive" }
    }

    private var consecutiveErrors = 0
    private var consecutiveUnknownFeed = 0

    /** Returns `null` to keep going, or why the capture ends. */
    fun onResult(code: Int): LoopExit? {
        when (code) {
            HfaCode.OK -> {
                consecutiveErrors = 0
                consecutiveUnknownFeed = 0
                return null
            }
            HfaCode.UNKNOWN_FEED -> {
                consecutiveErrors = 0
                consecutiveUnknownFeed++
                return if (consecutiveUnknownFeed >= maxUnknownFeed) LoopExit.NoSender else null
            }
            HfaCode.INVALID_ARGUMENT -> return LoopExit.PushFailed("the audio engine rejected the PCM format")
        }
        consecutiveUnknownFeed = 0
        consecutiveErrors++
        return if (consecutiveErrors >= maxConsecutiveErrors) {
            LoopExit.PushFailed("the audio engine failed $consecutiveErrors times in a row (code $code)")
        } else {
            null
        }
    }

    companion object {
        /** 50 pushes of 10 ms: half a second of continuous failures. */
        const val DEFAULT_MAX_CONSECUTIVE_ERRORS: Int = 50

        /** 200 pushes of 10 ms: two seconds without a sender reading the feed. */
        const val DEFAULT_MAX_UNKNOWN_FEED: Int = 200
    }
}

/**
 * The recording side of the capture loop, bound to one reused sample buffer.
 *
 * The Android implementation wraps an `AudioRecord` and a `FloatArray` or `ShortArray`.
 */
interface PcmPipe {
    /** Channels per frame of the buffer. */
    val channels: Int

    /**
     * Blocks until audio is read into the buffer. Returns the number of samples read (0 is
     * allowed, for example while stopping) or a negative `AudioRecord` error code.
     */
    fun read(): Int

    /** Pushes the first [frames] frames of the buffer; returns an [HfaCode]. */
    fun push(frames: Int): Int
}

/** Why [CaptureLoop.run] returned. */
sealed interface LoopExit {
    /** [CaptureLoop.run]'s `isRunning` became false: a requested stop. */
    data object Stopped : LoopExit

    /** Recording failed with the negative `AudioRecord` code [code]. */
    data class ReadFailed(val code: Int) : LoopExit

    /** The engine kept refusing the audio; [reason] comes from [PushPolicy]. */
    data class PushFailed(val reason: String) : LoopExit

    /** No sender has read the feed for a while (see [PushPolicy.DEFAULT_MAX_UNKNOWN_FEED]). */
    data object NoSender : LoopExit
}

/**
 * Reads chunks from a [PcmPipe] and pushes whole frames until stopped or failed.
 *
 * Runs on the capture thread. It never allocates per chunk: the pipe owns the buffer.
 */
class CaptureLoop(private val pipe: PcmPipe, private val policy: PushPolicy = PushPolicy()) {
    /** Runs while [isRunning] returns true; returns why it ended. */
    fun run(isRunning: () -> Boolean): LoopExit {
        while (isRunning()) {
            val samples = pipe.read()
            if (!isRunning()) break
            if (samples < 0) return LoopExit.ReadFailed(samples)
            val frames = PcmFormat.wholeFrames(samples, pipe.channels)
            if (frames == 0) continue
            val exit = policy.onResult(pipe.push(frames))
            if (exit != null) return exit
        }
        return LoopExit.Stopped
    }
}
