package com.rimeos.remote.phone

import android.app.Activity
import android.telephony.SmsManager
import com.rimeos.remote.core.phone.Messages
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Test

/** A send from a computer (contract §2.11) is sent only when every part says so. */
class SmsSenderTest {
    @Test
    fun `every part must go, and no signal is said as such`() {
        assertNull(SmsSender.verdict(listOf(Activity.RESULT_OK)))
        assertNull(SmsSender.verdict(listOf(Activity.RESULT_OK, Activity.RESULT_OK, Activity.RESULT_OK)))
        // The last part failing fails the whole message.
        assertEquals(Messages.FAILED, SmsSender.verdict(listOf(Activity.RESULT_OK, SmsManager.RESULT_ERROR_GENERIC_FAILURE)))
        assertEquals(Messages.NO_SERVICE, SmsSender.verdict(listOf(Activity.RESULT_OK, SmsManager.RESULT_ERROR_NO_SERVICE)))
        assertEquals(Messages.NO_SERVICE, SmsSender.verdict(listOf(SmsManager.RESULT_ERROR_RADIO_OFF)))
    }
}
