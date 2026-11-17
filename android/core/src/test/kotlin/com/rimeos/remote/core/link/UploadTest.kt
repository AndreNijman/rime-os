package com.rimeos.remote.core.link

import com.rimeos.remote.core.agent.Handoff
import java.io.ByteArrayInputStream
import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Test

/**
 * The phone's half of `Request::Receive` (P1-059 criterion 2).
 *
 * The daemon's half is asserted against a real `rime-agentd` and a real
 * `rime-remoted` in `rimed/rime-remoted/tests/end_to_end.rs`, which uploads
 * 200 KB over a real Noise channel and reads the file back off the disk. This
 * is the other end of the same wire, against [FakeMachine] — which can do the
 * two things the real one cannot be made to do on demand: hand the plug back
 * mid-upload, and split the closing reply across frames.
 */
class UploadTest {

    private fun body(n: Int): ByteArray = ByteArray(n) { (it * 31 % 251).toByte() }

    @Test
    fun `a file larger than one frame crosses in pieces and comes back as one path`() {
        // 200_000 bytes against `MAX_PAYLOAD`'s 65514. The criterion was
        // refused on the arithmetic of a single frame, so a test that sent
        // 40 KB would pass without touching the thing that was called
        // impossible.
        val bytes = body(200_000)
        val machine = FakeMachine(
            receiveReply = { """{"reply":"receiving","id":7,"len":200000}""" },
        )
        val landed = Upload.send(
            connect = { machine.open() },
            id = 7,
            name = "shot.png",
            len = bytes.size.toLong(),
            source = { ByteArrayInputStream(bytes) },
        )
        assertEquals(7, landed.session)
        assertEquals("/tmp/rime-agent-1000/7/inbox/001-shot.png", landed.path)
        assertArrayEquals(bytes, machine.uploadedBytes(), "the file that arrived is not the one sent")
        assertEquals(1, machine.uploadsStarted.get())
        // The request went in a `Frame.Open`, never on channel zero. The
        // proxy refuses a takeover verb there by name, so a client that sent
        // it as a control frame would wedge both ends.
        assertTrue(
            machine.requests.any { it.contains(""""cmd":"receive"""") && it.contains(""""len":200000""") },
            "the receive request is not what reached the machine: ${machine.requests.toList()}",
        )
    }

    @Test
    fun `the name crosses as the picker gave it, and the reduction stays the daemon's`() {
        // The phone does NOT sanitise. A second implementation of `safe_name`
        // here would be a thing that drifts, and the one case where it
        // differed is the case where the user is shown a filename that is not
        // the one on disk. `Handoff.Files.preview` shows the reduction without
        // performing it, and `safe-names.json` is what keeps the two the same.
        val machine = FakeMachine(receiveReply = { """{"reply":"receiving","id":7,"len":5}""" })
        Upload.send(
            connect = { machine.open() },
            id = 7,
            name = "../../.ssh/authorized_keys",
            len = 5,
            source = { ByteArrayInputStream("key\n\n".toByteArray()) },
        )
        val sent = machine.requests.first { it.contains(""""cmd":"receive"""") }
        assertTrue(
            sent.contains("""../../.ssh/authorized_keys"""),
            "the phone reduced the name itself: $sent",
        )
        assertEquals(".._.._.ssh_authorized_keys", Handoff.Files.preview("../../.ssh/authorized_keys"))
    }

    @Test
    fun `a reply split across frames is not read as a refusal`() {
        // `rime-remoted`'s pump frames whatever one read of the daemon's
        // socket returned, so the closing line can arrive in pieces. A client
        // that parsed the first piece would report "the machine refused" for a
        // file that was delivered — and the failure would depend on timing,
        // which is the shape of defect that passes on an idle laptop.
        val bytes = body(4096)
        val machine = FakeMachine(
            receiveReply = { """{"reply":"receiving","id":7,"len":4096}""" },
            replyChunkBytes = 9,
        )
        val landed = Upload.send(
            connect = { machine.open() },
            id = 7,
            name = "shot.png",
            len = bytes.size.toLong(),
            source = { ByteArrayInputStream(bytes) },
        )
        assertEquals("/tmp/rime-agent-1000/7/inbox/001-shot.png", landed.path)
    }

    @Test
    fun `a refused upload carries the machine's own sentence and sends nothing`() {
        // Everything refusable is decided before the takeover reply, so a
        // refusal must cost no bytes at all. This is the assertion that says
        // the round trip was not paid for.
        val machine = FakeMachine(
            receiveReply = { """{"reply":"error","kind":"no_such_session","message":"no session 7"}""" },
        )
        val e = assertThrows(Upload.Refused::class.java) {
            Upload.send(
                connect = { machine.open() },
                id = 7,
                name = "shot.png",
                len = 4096,
                source = { ByteArrayInputStream(body(4096)) },
            )
        }
        assertTrue(e.message!!.contains("no session 7"), "the refusal lost the machine's words: ${e.message}")
        assertEquals(0, machine.uploadedBytes().size, "bytes were sent to a machine that refused")
    }

    @Test
    fun `an error after the bytes arrived is a refusal and not a success`() {
        // The other refusal point. `deliver` re-checks the session UNDER ITS
        // LOCK, because a session can exit while two megabytes are in flight —
        // so a daemon that said `receiving` can still answer an error, and a
        // client that only looked for `injected` would hang waiting for one.
        val machine = FakeMachine(
            receiveReply = { """{"reply":"receiving","id":7,"len":64}""" },
            injectedReply = {
                """{"reply":"error","kind":"session_exited","message":"session 7 has already exited"}"""
            },
        )
        val e = assertThrows(Upload.Refused::class.java) {
            Upload.send(
                connect = { machine.open() },
                id = 7,
                name = "shot.png",
                len = 64,
                source = { ByteArrayInputStream(body(64)) },
            )
        }
        assertTrue(e.message!!.contains("already exited"), "${e.message}")
    }

    @Test
    fun `a connection that dies mid-upload is a failure and never a partial file`() {
        // A phone walking out of range. There is no resumption on a Noise
        // transport, so there is nothing to continue — what matters is that
        // the caller is told, rather than being left with a promise that a
        // file arrived.
        val machine = FakeMachine(receiveReply = { """{"reply":"receiving","id":7,"len":200000}""" })
        val thread = Thread {
            Thread.sleep(50)
            machine.hangUp()
        }
        thread.isDaemon = true
        thread.start()
        assertThrows(Disconnected::class.java) {
            Upload.send(
                connect = { machine.open() },
                id = 7,
                name = "shot.png",
                len = 200_000,
                source = {
                    // A stream that trickles, so the hangup lands mid-upload
                    // rather than after it.
                    object : java.io.InputStream() {
                        private val data = body(200_000)
                        private var at = 0
                        override fun read(): Int = if (at < data.size) data[at++].toInt() and 0xff else -1
                        override fun read(b: ByteArray, off: Int, len: Int): Int {
                            if (at >= data.size) return -1
                            Thread.sleep(2)
                            val n = minOf(len, 1024, data.size - at)
                            System.arraycopy(data, at, b, off, n)
                            at += n
                            return n
                        }
                    }
                },
            )
        }
    }

    @Test
    fun `a source shorter than the length it declared is refused rather than truncated`() {
        // The daemon committed to reading exactly `len` bytes. A client that
        // ran out and stopped would leave it waiting until its own stall
        // timeout, and a client that padded would hand the agent a corrupted
        // file it was told was a screenshot.
        val machine = FakeMachine(receiveReply = { """{"reply":"receiving","id":7,"len":4096}""" })
        val e = assertThrows(Upload.Refused::class.java) {
            Upload.send(
                connect = { machine.open() },
                id = 7,
                name = "shot.png",
                len = 4096,
                source = { ByteArrayInputStream(body(100)) },
            )
        }
        assertTrue(e.message!!.contains("100 of 4096"), "${e.message}")
    }

    @Test
    fun `the cap is applied here so a phone does not pay a round trip to learn it`() {
        // `inject::MAX_BYTES`. The daemon refuses a larger file before the
        // takeover reply — so this changes no outcome, only who pays for it.
        val machine = FakeMachine()
        assertThrows(IllegalArgumentException::class.java) {
            Upload.send(
                connect = { machine.open() },
                id = 7,
                name = "big.bin",
                len = Handoff.Files.MAX_BYTES + 1,
                source = { ByteArrayInputStream(ByteArray(0)) },
            )
        }
        assertThrows(IllegalArgumentException::class.java) {
            Upload.send(
                connect = { machine.open() },
                id = 7,
                name = "empty.bin",
                len = 0,
                source = { ByteArrayInputStream(ByteArray(0)) },
            )
        }
        assertEquals(0, machine.connections.get(), "a refused upload opened a connection anyway")
    }

    @Test
    fun `progress is reported as bytes leave rather than when the reply arrives`() {
        // A 2 MB photo on a mobile link is tens of seconds of nothing to look
        // at. The counter has to move while the bytes move, which means it is
        // called from inside the send loop and not after it.
        val bytes = body(150_000)
        val machine = FakeMachine(receiveReply = { """{"reply":"receiving","id":7,"len":150000}""" })
        val seen = ArrayList<Long>()
        Upload.send(
            connect = { machine.open() },
            id = 7,
            name = "shot.png",
            len = bytes.size.toLong(),
            source = { ByteArrayInputStream(bytes) },
            onProgress = { synchronized(seen) { seen.add(it) } },
        )
        assertTrue(seen.size > 1, "progress was reported once, which is not progress: $seen")
        assertEquals(bytes.size.toLong(), seen.last())
        assertEquals(seen.sorted(), seen, "progress went backwards: $seen")
    }

    // --- save_file: "Send to Rime" (contract §1.8) ---

    private val savedReply: (ByteArray) -> String = {
        """{"reply":"saved","name":"IMG_2041 (2).jpg","path":"/home/u/Downloads/IMG_2041 (2).jpg"}"""
    }

    @Test
    fun `a shared file crosses on its own channel and comes back with the name the computer chose`() {
        val bytes = body(200_000)
        val machine = FakeMachine(
            receiveReply = { """{"reply":"saving","len":200000}""" },
            injectedReply = savedReply,
            replyChunkBytes = 7,
        )
        val saved = Upload.save(
            connect = { machine.open() },
            name = "IMG_2041.jpg",
            len = bytes.size.toLong(),
            source = { ByteArrayInputStream(bytes) },
        )
        assertEquals("IMG_2041 (2).jpg", saved.name, "the numbered name is the daemon's, and the screen shows it")
        assertEquals("/home/u/Downloads/IMG_2041 (2).jpg", saved.path)
        assertArrayEquals(bytes, machine.uploadedBytes())
        val sent = machine.requests.single { it.contains(""""cmd":"save_file"""") }
        assertTrue(sent.contains(""""name":"IMG_2041.jpg"""") && sent.contains(""""len":200000"""), sent)
        assertTrue(machine.requests.none { it.contains(""""cmd":"receive"""") }, "a save went out as a receive")
    }

    @Test
    fun `a save answered with receiving is not taken for a save`() {
        // Each verb accepts only its own reply: a machine that answered
        // `receiving` to a `save_file` is not one this phone understands.
        val machine = FakeMachine(receiveReply = { """{"reply":"receiving","id":7,"len":5}""" })
        assertThrows(Upload.Refused::class.java) {
            Upload.save(connect = { machine.open() }, name = "a.txt", len = 5, source = { ByteArrayInputStream(body(5)) })
        }
    }

    @Test
    fun `a refused save carries the machine's words and sends no bytes`() {
        val machine = FakeMachine(
            receiveReply = { """{"reply":"error","kind":"bad_request","message":"not enough free space for 2.0 MB"}""" },
        )
        val e = assertThrows(Upload.Refused::class.java) {
            Upload.save(connect = { machine.open() }, name = "a.bin", len = 4096, source = { ByteArrayInputStream(body(4096)) })
        }
        assertTrue(e.message!!.contains("not enough free space"), "${e.message}")
        assertEquals(0, machine.uploadedBytes().size)
    }

    @Test
    fun `an error after the bytes is a refusal, never a saved file`() {
        val machine = FakeMachine(
            receiveReply = { """{"reply":"saving","len":64}""" },
            injectedReply = { """{"reply":"error","kind":"internal","message":"could not write the file"}""" },
        )
        val e = assertThrows(Upload.Refused::class.java) {
            Upload.save(connect = { machine.open() }, name = "a.bin", len = 64, source = { ByteArrayInputStream(body(64)) })
        }
        assertTrue(e.message!!.contains("could not write"), "${e.message}")
    }

    @Test
    fun `the save cap is the daemon's and is applied before a connection opens`() {
        val machine = FakeMachine()
        assertThrows(IllegalArgumentException::class.java) {
            Upload.save(
                connect = { machine.open() },
                name = "big.bin",
                len = Upload.SAVE_MAX_BYTES + 1,
                source = { ByteArrayInputStream(ByteArray(0)) },
            )
        }
        assertThrows(IllegalArgumentException::class.java) {
            Upload.save(connect = { machine.open() }, name = "e", len = 0, source = { ByteArrayInputStream(ByteArray(0)) })
        }
        assertEquals(0, machine.connections.get())
        // Larger than a session hand-off may be: a video is the point of this verb.
        assertTrue(Upload.SAVE_MAX_BYTES > Handoff.Files.MAX_BYTES)
    }

    @Test
    fun `a source longer than it declared is refused, and its last chunk is never sent`() {
        // A stale SIZE column. Sending `len` bytes and stopping would leave a
        // cut file on the computer that the phone called saved.
        val machine = FakeMachine(receiveReply = { """{"reply":"saving","len":4096}""" }, injectedReply = savedReply)
        val e = assertThrows(Upload.Refused::class.java) {
            Upload.save(connect = { machine.open() }, name = "a.bin", len = 4096, source = { ByteArrayInputStream(body(5000)) })
        }
        assertTrue(e.message!!.contains("longer than the 4096 bytes"), "${e.message}")
        assertTrue(machine.uploaded.isEmpty(), "the machine got every declared byte, so it would have saved a cut file")
    }
}
