package com.rimeos.remote.core.phone

import com.rimeos.remote.core.Base64Url
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/**
 * This phone's recent photos on a paired computer
 * (`docs/remote-live-contract.md` §2.13): what a row of Android's image store
 * becomes on the wire, how a thumbnail is made small and bare, how an answer
 * is cut into lines the link carries, and when this phone refuses.
 *
 * The computer asks with a `photos_request` or `photos_fetch` event on the
 * phone's events channel. This phone answers a list with `photos_reply`
 * lines on the control channel, and a fetch by uploading the original on a
 * connection of its own (`photos_upload`), or with one `photos_fetch_reply`
 * line saying why it will not. Nothing is kept here: each answer is read from
 * the phone's own store when it is asked for.
 *
 * Everything here is plain Kotlin, so the rules are tested on the JVM; the
 * app reads the store, draws the thumbnails and opens the originals.
 */
object Photos {
    /** The feature a machine's `remote_hello` lists when it asks for this phone's photos. */
    const val FEATURE: String = "phone_photos"

    /**
     * What the manifest declares for this feature, and nothing else from the
     * storage family: READ_EXTERNAL_STORAGE only up to Android 12 (the
     * manifest caps it), READ_MEDIA_IMAGES from 13, and from 14 the partial
     * "selected photos" grant. Never ACCESS_MEDIA_LOCATION, never video.
     */
    val PERMISSIONS: List<String> = listOf(
        "android.permission.READ_EXTERNAL_STORAGE",
        "android.permission.READ_MEDIA_IMAGES",
        "android.permission.READ_MEDIA_VISUAL_USER_SELECTED",
    )

    /** Most photos one request asks for. The computer caps it the same. */
    const val MAX_LIMIT: Int = 60

    /** Longest side of a thumbnail, in pixels. */
    const val THUMB_SIDE: Int = 256

    /** Largest thumbnail this phone sends, in bytes of JPEG. The computer takes up to 32 KiB. */
    const val THUMB_MAX_BYTES: Int = 24 * 1024

    /**
     * Largest answer line, in bytes. A wire frame holds 65,514; at 60,000 one
     * photo with the largest thumbnail (about 33 KB once base64url) always
     * fits a line on its own, with room for its name and the line's keys.
     */
    const val LINE_BYTES: Int = 60_000

    /** Most lines, and bytes, one answer may take on the computer. */
    const val MAX_LINES: Int = 64
    const val MAX_ANSWER_BYTES: Int = 2_621_440

    /** The largest original this phone sends; the computer refuses more before a byte crosses. */
    const val MAX_PHOTO_BYTES: Long = 64L * 1024 * 1024

    /** Longest name sent, in characters; the computer cuts it there too. */
    const val NAME_CAP: Int = 120

    /** Longest mime type sent, in ASCII characters, as the computer allows. */
    const val MIME_CAP: Int = 40

    /**
     * Rows of the store read for one listing, newest added first. Bounds the
     * work on a phone with a huge library; past it, the oldest added are the
     * ones left out.
     */
    const val SCAN_CAP: Int = 50_000

    // Why this phone did not answer, as the computer knows them (§2.13).
    const val OFF: String = "off"
    const val NO_PERMISSION: String = "no_permission"
    const val LOCKED: String = "locked"
    const val BUSY: String = "busy"
    const val NOT_FOUND: String = "not_found"
    const val TOO_LARGE: String = "too_large"
    const val FAILED: String = "failed"

    /** What Android lets this app read of the person's images. */
    enum class Access {
        /** Nothing: no grant at all. */
        NONE,

        /** Only the photos the person picked (Android 14 and later's "selected photos"). */
        PARTIAL,

        /** Every image on the phone. */
        FULL,
    }

    /** One row of the image store, as the app read it. Columns Android may leave empty are null. */
    data class Row(
        val id: Long,
        val name: String?,
        /** DATE_TAKEN, unix milliseconds, when the store knows it. */
        val dateTakenMs: Long?,
        /** DATE_ADDED, unix SECONDS: Android keeps this one in seconds. */
        val dateAddedS: Long?,
        val width: Int?,
        val height: Int?,
        val size: Long?,
        val mime: String?,
    ) {
        // A photo's name can say where and with whom: never in a log line by accident.
        override fun toString(): String = "Row(id=$id, mime=$mime)"
    }

    /** One photo as the computer is told it. [thumb] is a base64url JPEG, or null when none was made. */
    data class Photo(
        val id: Long,
        val name: String,
        val dateMs: Long,
        val width: Int,
        val height: Int,
        val size: Long,
        val mime: String,
        val thumb: String? = null,
    ) {
        override fun toString(): String = "Photo(id=$id, thumb=${thumb?.length})"
    }

    /**
     * Whether a machine is told it may ask (`can` has [Phone.CAN_PHOTOS]):
     * the person's switch is on, Android lets the app read images (all of
     * them or the ones picked), and the link is a full one on an unlocked
     * app. Never on a companion link: a photo is more than a battery level,
     * so a phone that restarted and was not unlocked shows none.
     */
    fun offered(switchOn: Boolean, access: Access, companion: Boolean, unlocked: Boolean): Boolean =
        refusal(switchOn, access, companion, unlocked) == null

    /**
     * Why this phone will not answer a list or a fetch now, or null when it
     * will. Checked on every request, whatever an older report told the
     * machine.
     */
    fun refusal(switchOn: Boolean, access: Access, companion: Boolean, unlocked: Boolean): String? = when {
        !switchOn -> OFF
        access == Access.NONE -> NO_PERMISSION
        companion || !unlocked -> LOCKED
        else -> null
    }

    /**
     * When a photo was taken, in unix milliseconds: DATE_TAKEN, or when the
     * store does not know it, DATE_ADDED (kept in seconds, so times 1,000).
     * Zero when neither is known.
     */
    fun dateMs(takenMs: Long?, addedS: Long?): Long = when {
        takenMs != null && takenMs > 0 -> takenMs
        addedS != null && addedS > 0 -> addedS * 1000
        else -> 0
    }

    /**
     * A row as the computer is told it, or null for one never shown: not an
     * image (a mime type outside `image/`), or one whose length the store
     * does not know yet (a file still being written or scanned), which could
     * not be fetched anyway. An unknown width or height is sent as 0.
     */
    fun photo(row: Row): Photo? {
        val mime = mime(row.mime) ?: return null
        val size = row.size?.takeIf { it > 0 } ?: return null
        return Photo(
            id = row.id,
            name = name(row.name),
            dateMs = dateMs(row.dateTakenMs, row.dateAddedS),
            width = (row.width ?: 0).coerceAtLeast(0),
            height = (row.height ?: 0).coerceAtLeast(0),
            size = size,
            mime = mime,
        )
    }

    /**
     * One page of the listing from [rows] (in any order): the newest [limit]
     * photos older than [before] (unix ms, exclusive), newest first by
     * [Photo.dateMs], then by id, highest first, so a page boundary never
     * splits photos the computer could not tell apart.
     */
    fun page(rows: Sequence<Row>, before: Long?, limit: Int): List<Photo> =
        rows.take(SCAN_CAP)
            .mapNotNull { photo(it) }
            .filter { before == null || it.dateMs < before }
            .sortedWith(compareByDescending<Photo> { it.dateMs }.thenByDescending { it.id })
            .take(limit.coerceIn(1, MAX_LIMIT))
            .toList()

    /** A mime type the computer accepts: `image/…`, lower case, short ASCII; else null. */
    fun mime(raw: String?): String? {
        val m = raw?.trim()?.lowercase() ?: return null
        if (m.length > MIME_CAP || !m.startsWith("image/") || m.length == "image/".length) return null
        return m.takeIf { it.all { c -> c in 'a'..'z' || c in '0'..'9' || c in "/.+-" } && it.count { c -> c == '/' } == 1 }
    }

    /**
     * A file name made safe to show and to save under: no folder, no control
     * or direction-changing characters, at most [NAME_CAP] characters, its
     * extension kept when it is cut. The computer reduces it again (§1.8)
     * and does not rely on this.
     */
    fun name(raw: String?): String {
        val base = (raw ?: "").substringAfterLast('/').substringAfterLast('\\')
        val clean = Messages.clean(base, Int.MAX_VALUE, 0).trim()
        if (clean.isEmpty()) return "photo"
        if (clean.codePointCount(0, clean.length) <= NAME_CAP) return clean
        val dot = clean.lastIndexOf('.')
        val ext = if (dot > 0 && clean.length - dot <= 10) clean.substring(dot) else ""
        val stem = clean.substring(0, clean.length - ext.length)
        val keep = NAME_CAP - ext.length
        return stem.substring(0, stem.offsetByCodePoints(0, keep)).trimEnd() + ext
    }

    /**
     * The size a thumbnail of a [width] x [height] image is drawn at: its
     * longest side [side], the other in proportion, never larger than the
     * image itself (a small image is not blown up). Null when the image has
     * no size to scale.
     */
    fun targetSize(width: Int, height: Int, side: Int = THUMB_SIDE): Pair<Int, Int>? {
        if (width <= 0 || height <= 0 || side <= 0) return null
        val long = maxOf(width, height)
        if (long <= side) return width to height
        val scale = side.toDouble() / long
        val w = if (width >= height) side else Math.round(width * scale).toInt().coerceIn(1, side)
        val h = if (height > width) side else Math.round(height * scale).toInt().coerceIn(1, side)
        return w to h
    }

    /**
     * The order thumbnails are tried in, as (longest side, JPEG quality):
     * the full size at falling quality first, then smaller sizes, until one
     * fits [THUMB_MAX_BYTES]. A picture full of detail still gets a
     * thumbnail; a smaller one is better than a blurred one.
     */
    val THUMB_PLAN: List<Pair<Int, Int>> =
        listOf(THUMB_SIDE, 192, 128).flatMap { side -> listOf(80, 70, 60, 50, 40).map { q -> side to q } }

    /**
     * A thumbnail for the wire: [encode] draws the photo at a longest side
     * and JPEG quality (null when it cannot), each try in [THUMB_PLAN]'s
     * order; the first that is a JPEG and fits [THUMB_MAX_BYTES] once its
     * metadata is gone is sent, base64url without padding. Null when none
     * fits, and the photo is listed without one.
     */
    fun thumb(encode: (side: Int, quality: Int) -> ByteArray?): String? {
        for ((side, quality) in THUMB_PLAN) {
            val bytes = encode(side, quality) ?: return null
            val bare = Jpeg.strip(bytes) ?: return null
            if (bare.size <= THUMB_MAX_BYTES) return Base64Url.encode(bare)
        }
        return null
    }

    /**
     * The `photos_reply` lines answering request [id]: [photos] in order,
     * as many to a line as fit [LINE_BYTES] (one always fits); every line
     * but the last says `more`, and every line says `partial` when the
     * person let the app see only the photos they picked. No photos is one
     * line with an empty list.
     */
    fun reply(id: Long, photos: List<Photo>, partial: Boolean): List<String> {
        val items = photos.map { encode(it) }
        // A line's size is its keys plus each item and the commas between,
        // measured with `more` on, which is the longer of the two.
        val base = line(id, emptyList(), more = true, partial = partial).toByteArray(Charsets.UTF_8).size
        val groups = ArrayList<List<JsonObject>>()
        var current = ArrayList<JsonObject>()
        var size = base
        for (item in items) {
            val n = item.toString().toByteArray(Charsets.UTF_8).size
            if (current.isNotEmpty() && size + n + 1 > LINE_BYTES) {
                groups += current
                current = ArrayList()
                size = base
            }
            size += if (current.isEmpty()) n else n + 1
            current += item
        }
        groups += current
        return groups.mapIndexed { i, g -> line(id, g, more = i < groups.size - 1, partial = partial) }
    }

    /** The one line refusing list request [id] with [word]. */
    fun refused(id: Long, word: String): String =
        buildJsonObject {
            put("cmd", "photos_reply")
            put("id", id)
            put("error", word)
        }.toString()

    /** The one line saying this phone will not send photo fetch [id], with [word]. */
    fun fetchRefused(id: Long, word: String): String =
        buildJsonObject {
            put("cmd", "photos_fetch_reply")
            put("id", id)
            put("error", word)
        }.toString()

    /** The `photos_upload` channel request carrying fetch [id]'s original: [len] bytes named [name]. */
    fun uploadRequest(id: Long, name: String, len: Long): String =
        buildJsonObject {
            put("cmd", "photos_upload")
            put("id", id)
            put("name", name)
            put("len", len)
        }.toString()

    private fun encode(p: Photo): JsonObject = buildJsonObject {
        put("id", p.id)
        put("name", p.name)
        put("date_ms", p.dateMs)
        put("width", p.width)
        put("height", p.height)
        put("size", p.size)
        put("mime", p.mime)
        if (p.thumb != null) put("thumb", p.thumb)
    }

    private fun line(id: Long, items: List<JsonObject>, more: Boolean, partial: Boolean): String =
        buildJsonObject {
            put("cmd", "photos_reply")
            put("id", id)
            put("photos", JsonArray(items))
            if (more) put("more", true)
            if (partial) put("partial", true)
        }.toString()

    /**
     * A JPEG's metadata, taken out.
     *
     * Bitmap.compress writes no EXIF, so a thumbnail this phone draws has
     * none to begin with. This is the guarantee that does not depend on that:
     * every APP1 to APP15 segment (EXIF, XMP, ICC profiles, maker notes) and
     * every comment goes, and everything a decoder needs (APP0, the tables,
     * the frame header, the scan) is copied byte for byte.
     */
    object Jpeg {
        private const val SOI = 0xD8
        private const val EOI = 0xD9
        private const val SOS = 0xDA
        private const val COM = 0xFE
        private const val TEM = 0x01

        /**
         * [jpeg] without its metadata segments, or null when it is not a
         * JPEG this can walk (no start marker, a segment that runs off the
         * end, no scan). Segments are walked up to the first scan; the scan
         * and what follows are copied as they are, since entropy-coded data
         * is not made of segments.
         */
        fun strip(jpeg: ByteArray): ByteArray? {
            val n = jpeg.size
            if (n < 4 || u(jpeg, 0) != 0xFF || u(jpeg, 1) != SOI) return null
            val out = java.io.ByteArrayOutputStream(n)
            out.write(jpeg, 0, 2)
            var i = 2
            while (i < n) {
                if (u(jpeg, i) != 0xFF) return null
                // Any number of 0xFF fill bytes may come before a marker.
                var m = i + 1
                while (m < n && u(jpeg, m) == 0xFF) m++
                if (m >= n) return null
                val marker = u(jpeg, m)
                // An end before any scan is no picture at all.
                if (marker == EOI) return null
                if (marker == TEM || marker in 0xD0..0xD7) {
                    out.write(0xFF)
                    out.write(marker)
                    i = m + 1
                    continue
                }
                if (m + 2 >= n) return null
                val len = (u(jpeg, m + 1) shl 8) or u(jpeg, m + 2)
                val end = m + 1 + len
                if (len < 2 || end > n) return null
                if (marker == SOS) {
                    out.write(0xFF)
                    out.write(jpeg, m, n - m)
                    return out.toByteArray()
                }
                val metadata = marker in 0xE1..0xEF || marker == COM
                if (!metadata) {
                    out.write(0xFF)
                    out.write(jpeg, m, end - m)
                }
                i = end
            }
            return null
        }

        private fun u(b: ByteArray, i: Int): Int = b[i].toInt() and 0xFF
    }
}
