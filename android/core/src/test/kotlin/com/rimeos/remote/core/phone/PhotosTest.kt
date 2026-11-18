package com.rimeos.remote.core.phone

import com.rimeos.remote.core.Base64Url
import java.awt.image.BufferedImage
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import javax.imageio.ImageIO
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long
import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/** This phone's photos on a computer (`docs/remote-live-contract.md` §2.13). */
class PhotosTest {
    private fun row(
        id: Long,
        name: String? = "IMG_$id.jpg",
        taken: Long? = 1_000L * id,
        addedS: Long? = id,
        width: Int? = 4000,
        height: Int? = 3000,
        size: Long? = 3_000_000,
        mime: String? = "image/jpeg",
    ) = Photos.Row(id, name, taken, addedS, width, height, size, mime)

    private fun photo(id: Long, thumb: String? = null, name: String = "IMG_$id.jpg") =
        Photos.Photo(id, name, 1_763_452_800_000 - id, 4080, 3072, 3_145_728, "image/jpeg", thumb)

    private fun parse(line: String): JsonObject = Json.parseToJsonElement(line).jsonObject

    private fun ids(lines: List<String>): List<Long> =
        lines.flatMap { l -> parse(l)["photos"]!!.jsonArray.map { it.jsonObject["id"]!!.jsonPrimitive.long } }

    // ---- rows ------------------------------------------------------------

    @Test
    fun `a photo is dated when it was taken, or when it was added, which Android keeps in seconds`() {
        assertEquals(1_763_452_800_123, Photos.photo(row(1, taken = 1_763_452_800_123, addedS = 5))!!.dateMs)
        assertEquals(1_763_452_800_000, Photos.photo(row(2, taken = null, addedS = 1_763_452_800))!!.dateMs)
        // Some phones write zero rather than nothing.
        assertEquals(1_763_452_800_000, Photos.photo(row(3, taken = 0, addedS = 1_763_452_800))!!.dateMs)
        assertEquals(0, Photos.photo(row(4, taken = null, addedS = null))!!.dateMs)
    }

    @Test
    fun `only images are listed, and a row with no length yet is left out`() {
        assertNull(Photos.photo(row(1, mime = "video/mp4")))
        assertNull(Photos.photo(row(2, mime = null)))
        assertNull(Photos.photo(row(3, mime = "image/")))
        assertNull(Photos.photo(row(4, mime = "image/jp\u0000eg")))
        assertNull(Photos.photo(row(5, mime = "image/" + "x".repeat(40))))
        assertNull(Photos.photo(row(6, size = null)), "a file still being scanned has no length to send")
        assertNull(Photos.photo(row(7, size = 0)))
        assertEquals("image/heic", Photos.photo(row(8, mime = " IMAGE/HEIC "))!!.mime)
        assertEquals("image/svg+xml", Photos.photo(row(9, mime = "image/svg+xml"))!!.mime)
        // An unknown size is sent as 0, never as a negative or a null.
        val p = Photos.photo(row(10, width = null, height = -1))!!
        assertEquals(0, p.width)
        assertEquals(0, p.height)
    }

    @Test
    fun `a name loses its folder and its control characters and keeps its extension when cut`() {
        assertEquals("IMG_1.jpg", Photos.photo(row(1, name = "DCIM/Camera/IMG_1.jpg"))!!.name)
        assertEquals("a b.jpg", Photos.name("a\nb.jpg"))
        assertEquals("evil.jpg", Photos.name("‮evil.jpg"))
        assertEquals("photo", Photos.name(null))
        assertEquals("photo", Photos.name("  \u0007 "))
        val long = Photos.name("x".repeat(300) + ".jpeg")
        assertEquals(Photos.NAME_CAP, long.codePointCount(0, long.length))
        assertTrue(long.endsWith(".jpeg"), long)
        // Counted in characters, never cutting one in half.
        val wide = Photos.name("📷".repeat(200) + ".png")
        assertEquals(Photos.NAME_CAP, wide.codePointCount(0, wide.length))
        assertTrue(wide.endsWith(".png"))
    }

    @Test
    fun `a page is the newest photos before a time, newest first, ties by id`() {
        val rows = sequenceOf(
            row(1, taken = 5_000),
            row(2, taken = null, addedS = 9), // 9,000 by its added date
            row(3, taken = 7_000),
            row(4, taken = 7_000),
            row(5, taken = 1_000, mime = "video/mp4"),
            row(6, taken = 8_000),
        )
        assertEquals(listOf(2L, 6L, 4L, 3L, 1L), Photos.page(rows, null, 30).map { it.id })
        assertEquals(listOf(6L, 4L), Photos.page(rows, null, 30).drop(1).take(2).map { it.id })
        // `before` is exclusive: the photo at exactly 8,000 is on the page that asked for it.
        assertEquals(listOf(4L, 3L, 1L), Photos.page(rows, 8_000, 30).map { it.id })
        assertEquals(listOf(2L), Photos.page(rows, null, 1).map { it.id })
        assertEquals(listOf(2L), Photos.page(rows, null, 0).map { it.id }, "a limit is at least one")
        val many = (1..200L).asSequence().map { row(it) }
        assertEquals(Photos.MAX_LIMIT, Photos.page(many, null, 500).size)
    }

    // ---- thumbnails ------------------------------------------------------

    @Test
    fun `a thumbnail is at most 256 on its long side, in proportion, and never blown up`() {
        assertEquals(256 to 192, Photos.targetSize(4000, 3000))
        assertEquals(192 to 256, Photos.targetSize(3000, 4000))
        assertEquals(256 to 256, Photos.targetSize(1000, 1000))
        assertEquals(256 to 1, Photos.targetSize(10_000, 3), "a sliver keeps a pixel")
        assertEquals(100 to 50, Photos.targetSize(100, 50), "a small image is not enlarged")
        assertEquals(256 to 100, Photos.targetSize(256, 100))
        assertEquals(128 to 96, Photos.targetSize(4000, 3000, side = 128))
        assertNull(Photos.targetSize(0, 100))
        assertNull(Photos.targetSize(100, -1))
    }

    @Test
    fun `thumbnails are tried smaller and plainer until one fits, and sent bare`() {
        val asked = ArrayList<Pair<Int, Int>>()
        val fits = jpeg(16, 12, exif = true)
        val thumb = Photos.thumb { side, quality ->
            asked += side to quality
            if (side == 192 && quality == 60) fits else jpeg(16, 12, padding = Photos.THUMB_MAX_BYTES + 100)
        }
        assertNotNull(thumb)
        assertEquals(256 to 80, asked.first(), "the full size at good quality first")
        assertEquals(192 to 60, asked.last())
        assertTrue(asked.indexOf(256 to 40) < asked.indexOf(192 to 80), "every quality at one size before a smaller size")
        val bytes = Base64Url.decode(thumb!!)!!
        assertFalse(String(bytes, Charsets.ISO_8859_1).contains("Exif"), "the thumbnail carried its metadata")
        assertFalse(thumb.contains('=') || thumb.contains('+') || thumb.contains('/'), "base64url without padding")

        assertNull(Photos.thumb { _, _ -> jpeg(16, 12, padding = Photos.THUMB_MAX_BYTES + 100) }, "none fits: no thumbnail")
        assertNull(Photos.thumb { _, _ -> "not a jpeg at all".toByteArray() })
        assertNull(Photos.thumb { _, _ -> null })
        // Its metadata does not count against it: a picture that fits once bare is sent.
        val heavy = jpeg(16, 12, exif = true, comment = "z".repeat(Photos.THUMB_MAX_BYTES))
        assertNotNull(Photos.thumb { _, _ -> heavy })
    }

    // ---- the JPEG stripper -------------------------------------------------

    @Test
    fun `EXIF with a location is taken out and everything else is byte for byte the same`() {
        val plain = jpeg(32, 24)
        val tagged = jpeg(32, 24, exif = true, comment = "taken at home")
        assertTrue(String(tagged, Charsets.ISO_8859_1).contains("Exif"), "the fixture has no EXIF to remove")
        assertTrue(contains(tagged, GPS_LATITUDE), "the fixture has no GPS to remove")
        val stripped = Photos.Jpeg.strip(tagged)!!
        assertArrayEquals(plain, stripped, "the stripped picture differs from the same picture never tagged")
        assertFalse(String(stripped, Charsets.ISO_8859_1).contains("Exif"))
        assertFalse(contains(stripped, GPS_LATITUDE), "the latitude is still in it")
        assertFalse(String(stripped, Charsets.ISO_8859_1).contains("taken at home"), "the comment is still in it")
        assertFalse(markers(stripped).any { it in 0xE1..0xEF || it == 0xFE }, "a metadata segment is left: ${markers(stripped)}")
        assertTrue(0xE0 in markers(stripped), "APP0 (JFIF) is kept")
        // Still a picture.
        val image = ImageIO.read(ByteArrayInputStream(stripped))
        assertEquals(32, image.width)
        assertEquals(24, image.height)
        // And stripping again changes nothing.
        assertArrayEquals(stripped, Photos.Jpeg.strip(stripped))
    }

    @Test
    fun `every APPn but APP0 goes, fill bytes are dropped, and a non-JPEG is refused`() {
        val plain = jpeg(8, 8)
        val withAll = insertAfterApp0(
            plain,
            (0xE2..0xEF).flatMap { segment(it, "app$it".toByteArray()).toList() }.toByteArray() +
                byteArrayOf(0xFF.toByte(), 0xFF.toByte()) + segment(0xFE, "comment".toByteArray()),
        )
        assertArrayEquals(plain, Photos.Jpeg.strip(withAll))
        assertNull(Photos.Jpeg.strip("GIF89a............".toByteArray()))
        assertNull(Photos.Jpeg.strip(ByteArray(0)))
        assertNull(Photos.Jpeg.strip(byteArrayOf(0xFF.toByte(), 0xD8.toByte())))
        // A segment that says it runs past the end.
        assertNull(Photos.Jpeg.strip(byteArrayOf(0xFF.toByte(), 0xD8.toByte(), 0xFF.toByte(), 0xE1.toByte(), 0x7F, 0x00, 1, 2)))
        // An end before any scan is no picture.
        assertNull(Photos.Jpeg.strip(byteArrayOf(0xFF.toByte(), 0xD8.toByte(), 0xFF.toByte(), 0xD9.toByte())))
        // Garbage where a marker should be.
        assertNull(Photos.Jpeg.strip(byteArrayOf(0xFF.toByte(), 0xD8.toByte(), 0x12, 0x34, 0x56)))
    }

    // ---- answers -----------------------------------------------------------

    @Test
    fun `an answer is cut into lines of at most 60,000 bytes, all but the last saying more`() {
        val thumb = "A".repeat(32_000) // a 24 KiB JPEG in base64url
        val photos = (1..10L).map { photo(it, thumb) }
        val lines = Photos.reply(7, photos, partial = false)
        assertEquals(10, lines.size, "two large thumbnails never share a line")
        for ((i, l) in lines.withIndex()) {
            assertTrue(l.toByteArray().size <= Photos.LINE_BYTES, "line $i is ${l.toByteArray().size} bytes")
            val o = parse(l)
            assertEquals("photos_reply", o["cmd"]!!.jsonPrimitive.content)
            assertEquals(7, o["id"]!!.jsonPrimitive.long)
            assertEquals(i < lines.size - 1, o["more"]?.jsonPrimitive?.boolean ?: false, "line $i")
            assertNull(o["partial"], "partial only when the person picked photos")
        }
        assertEquals((1..10L).toList(), ids(lines), "every photo, once, in order")
    }

    @Test
    fun `small photos share lines, and the lines are filled greedily`() {
        val photos = (1..60L).map { photo(it, "B".repeat(5_000)) }
        val lines = Photos.reply(1, photos, partial = false)
        assertTrue(lines.size in 6..7, "${lines.size} lines")
        lines.forEach { assertTrue(it.toByteArray().size <= Photos.LINE_BYTES) }
        // No line could have taken the next line's first photo.
        for (i in 0 until lines.size - 1) {
            val next = parse(lines[i + 1])["photos"]!!.jsonArray.first().toString().toByteArray().size
            assertTrue(lines[i].toByteArray().size + next + 1 > Photos.LINE_BYTES, "line $i had room for more")
        }
        assertEquals((1..60L).toList(), ids(lines))
    }

    @Test
    fun `the worst answer fits the computer's 64 lines and its byte cap`() {
        // 60 photos, each with the largest thumbnail this phone sends and the
        // longest name, in four-byte characters.
        val thumb = Base64Url.encode(ByteArray(Photos.THUMB_MAX_BYTES) { 0x55 })
        val name = Photos.name("📷".repeat(500) + ".jpg")
        val photos = (1..60L).map { photo(it, thumb, name) }
        val lines = Photos.reply(9, photos, partial = true)
        assertTrue(lines.size <= Photos.MAX_LINES, "${lines.size} lines")
        val total = lines.sumOf { it.toByteArray().size }
        assertTrue(total <= Photos.MAX_ANSWER_BYTES, "$total bytes")
        lines.forEach { assertTrue(it.toByteArray().size <= Photos.LINE_BYTES, "${it.toByteArray().size}") }
        assertEquals(60, ids(lines).size)
    }

    @Test
    fun `partial access says so on every line, and an empty answer is one line`() {
        val lines = Photos.reply(3, (1..5L).map { photo(it, "C".repeat(30_000)) }, partial = true)
        assertTrue(lines.size > 1)
        lines.forEach { assertTrue(parse(it)["partial"]!!.jsonPrimitive.boolean, it.take(80)) }
        val empty = Photos.reply(4, emptyList(), partial = false)
        assertEquals(listOf("""{"cmd":"photos_reply","id":4,"photos":[]}"""), empty)
        assertEquals(
            listOf("""{"cmd":"photos_reply","id":4,"photos":[],"partial":true}"""),
            Photos.reply(4, emptyList(), partial = true),
        )
    }

    @Test
    fun `a photo carries its fields, and a missing thumbnail is left out`() {
        val o = parse(Photos.reply(1, listOf(photo(1000004211, "dGh1bWI")), false).single())["photos"]!!.jsonArray.single().jsonObject
        assertEquals(1000004211, o["id"]!!.jsonPrimitive.long)
        assertEquals("IMG_1000004211.jpg", o["name"]!!.jsonPrimitive.content)
        assertEquals(1_763_452_800_000 - 1000004211, o["date_ms"]!!.jsonPrimitive.long)
        assertEquals(4080, o["width"]!!.jsonPrimitive.int)
        assertEquals(3072, o["height"]!!.jsonPrimitive.int)
        assertEquals(3_145_728, o["size"]!!.jsonPrimitive.long)
        assertEquals("image/jpeg", o["mime"]!!.jsonPrimitive.content)
        assertEquals("dGh1bWI", o["thumb"]!!.jsonPrimitive.content)
        val bare = parse(Photos.reply(1, listOf(photo(2)), false).single())["photos"]!!.jsonArray.single().jsonObject
        assertFalse(bare.containsKey("thumb"))
    }

    @Test
    fun `refusals and the upload request are the shapes the computer knows`() {
        assertEquals("""{"cmd":"photos_reply","id":7,"error":"off"}""", Photos.refused(7, Photos.OFF))
        assertEquals("""{"cmd":"photos_fetch_reply","id":9,"error":"not_found"}""", Photos.fetchRefused(9, Photos.NOT_FOUND))
        assertEquals(
            """{"cmd":"photos_upload","id":9,"name":"PXL \"1\".jpg","len":3145728}""",
            Photos.uploadRequest(9, "PXL \"1\".jpg", 3_145_728),
        )
        assertEquals(
            setOf("off", "no_permission", "locked", "busy", "not_found", "too_large", "failed"),
            setOf(Photos.OFF, Photos.NO_PERMISSION, Photos.LOCKED, Photos.BUSY, Photos.NOT_FOUND, Photos.TOO_LARGE, Photos.FAILED),
        )
    }

    // ---- gates -------------------------------------------------------------

    @Test
    fun `the switch, the access, and a full unlocked link decide, in that order`() {
        val full = Photos.Access.FULL
        assertTrue(Photos.offered(true, full, companion = false, unlocked = true))
        assertTrue(Photos.offered(true, Photos.Access.PARTIAL, companion = false, unlocked = true), "picked photos are still photos")
        assertFalse(Photos.offered(false, full, companion = false, unlocked = true))
        assertFalse(Photos.offered(true, Photos.Access.NONE, companion = false, unlocked = true))
        assertFalse(Photos.offered(true, full, companion = true, unlocked = true), "never on a companion link")
        assertFalse(Photos.offered(true, full, companion = false, unlocked = false))

        assertEquals(Photos.OFF, Photos.refusal(false, Photos.Access.NONE, companion = true, unlocked = false))
        assertEquals(Photos.NO_PERMISSION, Photos.refusal(true, Photos.Access.NONE, companion = true, unlocked = false))
        assertEquals(Photos.LOCKED, Photos.refusal(true, full, companion = true, unlocked = true))
        assertEquals(Photos.LOCKED, Photos.refusal(true, full, companion = false, unlocked = false))
        assertNull(Photos.refusal(true, Photos.Access.PARTIAL, companion = false, unlocked = true))
    }

    @Test
    fun `the computer's events are read strictly`() {
        assertEquals(DeviceEvent.PhotosRequest(7, null, 30), Phone.event("""{"event":"photos_request","id":7,"limit":30}"""))
        assertEquals(
            DeviceEvent.PhotosRequest(8, 1_763_452_800_000, 30),
            Phone.event("""{"event":"photos_request","id":8,"before":1763452800000,"limit":30}"""),
        )
        assertEquals(DeviceEvent.PhotosRequest(1, null, Photos.MAX_LIMIT), Phone.event("""{"event":"photos_request","id":1,"limit":500}"""))
        assertNull(Phone.event("""{"event":"photos_request","id":1,"limit":0}"""))
        assertNull(Phone.event("""{"event":"photos_request","id":1}"""))
        assertNull(Phone.event("""{"event":"photos_request","id":-1,"limit":5}"""))
        assertNull(Phone.event("""{"event":"photos_request","id":1,"limit":"5"}"""))
        assertNull(Phone.event("""{"event":"photos_request","id":1,"limit":5,"before":"yesterday"}"""))
        assertNull(Phone.event("""{"event":"photos_request","id":1,"limit":5,"before":-3}"""))

        assertEquals(DeviceEvent.PhotosFetch(9, 1000004211), Phone.event("""{"event":"photos_fetch","id":9,"photo":1000004211}"""))
        assertNull(Phone.event("""{"event":"photos_fetch","id":9}"""))
        assertNull(Phone.event("""{"event":"photos_fetch","id":9,"photo":"1000004211"}"""))
        assertNull(Phone.event("""{"event":"photos_fetch","id":9,"photo":-1}"""))
        assertNull(Phone.event("""{"event":"photos_fetch","photo":1}"""))
    }

    // ---- fixtures ------------------------------------------------------------

    /**
     * A real JPEG from the JDK's encoder (SOI, APP0, DQT, SOF0, DHT, SOS …
     * EOI), optionally with an EXIF segment carrying a GPS latitude, a
     * comment, or padding (extra APP0 bytes, which stay after stripping)
     * put in after the first APP0.
     */
    private fun jpeg(w: Int, h: Int, exif: Boolean = false, comment: String? = null, padding: Int = 0): ByteArray {
        val image = BufferedImage(w, h, BufferedImage.TYPE_INT_RGB)
        for (y in 0 until h) for (x in 0 until w) image.setRGB(x, y, (x * 7 shl 16) or (y * 9 shl 8) or 0x40)
        val out = ByteArrayOutputStream()
        check(ImageIO.write(image, "jpg", out)) { "the JDK has no JPEG writer" }
        val plain = out.toByteArray()
        var extra = ByteArray(0)
        if (exif) extra += segment(0xE1, EXIF)
        if (comment != null) extra += segment(0xFE, comment.toByteArray())
        var left = padding
        while (left > 0) {
            val n = minOf(left, 60_000)
            // APP0, which the stripper keeps: bulk that is not metadata.
            extra += segment(0xE0, ByteArray(n) { 0x11 })
            left -= n
        }
        return if (extra.isEmpty()) plain else insertAfterApp0(plain, extra)
    }

    private fun segment(marker: Int, payload: ByteArray): ByteArray {
        val len = payload.size + 2
        return byteArrayOf(0xFF.toByte(), marker.toByte(), (len shr 8).toByte(), len.toByte()) + payload
    }

    private fun insertAfterApp0(jpeg: ByteArray, extra: ByteArray): ByteArray {
        check(jpeg[2] == 0xFF.toByte() && jpeg[3] == 0xE0.toByte()) { "the JDK's JPEG did not start with APP0" }
        val app0End = 4 + (((jpeg[4].toInt() and 0xFF) shl 8) or (jpeg[5].toInt() and 0xFF))
        return jpeg.copyOfRange(0, app0End) + extra + jpeg.copyOfRange(app0End, jpeg.size)
    }

    /** Every marker up to the scan. */
    private fun markers(jpeg: ByteArray): List<Int> {
        val out = ArrayList<Int>()
        var i = 2
        while (i + 3 < jpeg.size) {
            val m = jpeg[i + 1].toInt() and 0xFF
            out += m
            if (m == 0xDA) break
            i += 2 + (((jpeg[i + 2].toInt() and 0xFF) shl 8) or (jpeg[i + 3].toInt() and 0xFF))
        }
        return out
    }

    private fun contains(hay: ByteArray, needle: ByteArray): Boolean =
        (0..hay.size - needle.size).any { i -> needle.indices.all { hay[i + it] == needle[it] } }

    private companion object {
        /** 31° 57' 12.34", as three big-endian rationals: the bytes a location is. */
        val GPS_LATITUDE: ByteArray = byteArrayOf(
            0, 0, 0, 31, 0, 0, 0, 1,
            0, 0, 0, 57, 0, 0, 0, 1,
            0, 0, 0x04, 0xD2.toByte(), 0, 0, 0, 100,
        )

        /**
         * An EXIF payload: "Exif\0\0", a big-endian TIFF header, IFD0 with one
         * entry (GPSInfo, pointing at the GPS IFD), and a GPS IFD with
         * GPSLatitudeRef "N" and GPSLatitude, whose rationals follow it.
         */
        val EXIF: ByteArray = run {
            val b = ByteArrayOutputStream()
            fun u16(v: Int) { b.write(v shr 8); b.write(v) }
            fun u32(v: Int) { u16(v ushr 16); u16(v and 0xFFFF) }
            b.write("Exif".toByteArray()); b.write(0); b.write(0)
            // TIFF header: offsets below are from here.
            b.write("MM".toByteArray()); u16(42); u32(8)
            // IFD0 at 8: one entry, GPSInfo (0x8825), LONG, 1, the GPS IFD's offset (26).
            u16(1); u16(0x8825); u16(4); u32(1); u32(26); u32(0)
            // GPS IFD at 26: two entries.
            u16(2)
            u16(0x0001); u16(2); u32(2); b.write('N'.code); b.write(0); b.write(0); b.write(0)
            u16(0x0002); u16(5); u32(3); u32(56)
            u32(0)
            // GPSLatitude at 56.
            b.write(GPS_LATITUDE)
            b.toByteArray()
        }
    }
}
