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
 * - [HfaCode.OK] resets the error count.
 * - [HfaCode.UNKNOWN_FEED] drops the audio: no sender reads the feed right now.
 * - [HfaCode.INVALID_ARGUMENT] fails at once: the format was rejected and will not change.
 * - Any other code counts as a transient error; [maxConsecutiveErrors] of them in a row fail.
 */
class PushPolicy(private val maxConsecutiveErrors: Int = DEFAULT_MAX_CONSECUTIVE_ERRORS) {
    init {
        require(maxConsecutiveErrors > 0) { "maxConsecutiveErrors must be positive" }
    }

    private var consecutiveErrors = 0

    /** Returns `null` to keep going, or the reason to stop capturing. */
    fun onResult(code: Int): String? {
        when (code) {
            HfaCode.OK, HfaCode.UNKNOWN_FEED -> {
                consecutiveErrors = 0
                return null
            }
            HfaCode.INVALID_ARGUMENT -> return "the audio engine rejected the PCM format"
        }
        consecutiveErrors++
        return if (consecutiveErrors >= maxConsecutiveErrors) {
            "the audio engine failed $consecutiveErrors times in a row (code $code)"
        } else {
            null
        }
    }

    companion object {
        /** 50 pushes of 10 ms: half a second of continuous failures. */
        const val DEFAULT_MAX_CONSECUTIVE_ERRORS: Int = 50
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
            val reason = policy.onResult(pipe.push(frames))
            if (reason != null) return LoopExit.PushFailed(reason)
        }
        return LoopExit.Stopped
    }
}
