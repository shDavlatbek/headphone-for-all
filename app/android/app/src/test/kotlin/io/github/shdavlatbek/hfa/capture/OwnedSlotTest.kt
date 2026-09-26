package io.github.shdavlatbek.hfa.capture

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class OwnedSlotTest {
    private val oldEngine = Any()
    private val newEngine = Any()
    private val slot = OwnedSlot<String>()

    @Test
    fun startsEmpty() {
        assertNull(slot.value)
        assertFalse(slot.clear(oldEngine))
    }

    @Test
    fun theOwnerClearsItsValue() {
        slot.set(oldEngine, "old sink")
        assertEquals("old sink", slot.value)
        assertTrue(slot.clear(oldEngine))
        assertNull(slot.value)
    }

    @Test
    fun aLateCancelOfAnOldOwnerKeepsTheNewValue() {
        slot.set(oldEngine, "old sink")
        slot.set(newEngine, "new sink") // the new activity listens first
        assertFalse(slot.clear(oldEngine)) // then the old engine is cleaned up
        assertEquals("new sink", slot.value)
        assertTrue(slot.clear(newEngine))
        assertNull(slot.value)
    }

    @Test
    fun ownersCompareByIdentity() {
        val a = listOf(1)
        val b = listOf(1) // equal, but another engine
        slot.set(a, "sink")
        assertFalse(slot.clear(b))
        assertEquals("sink", slot.value)
    }
}
