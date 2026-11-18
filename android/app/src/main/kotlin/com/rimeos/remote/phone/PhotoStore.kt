package com.rimeos.remote.phone

import android.Manifest
import android.content.ContentUris
import android.content.Context
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Matrix
import android.net.Uri
import android.os.Build
import android.provider.MediaStore
import android.util.Log
import android.util.Size
import androidx.core.content.ContextCompat
import com.rimeos.remote.core.phone.Photos
import java.io.ByteArrayOutputStream
import java.io.InputStream

/**
 * This phone's images, read for a paired computer when it asks
 * (contract §2.13). Nothing read is kept: each listing is a fresh query, each
 * thumbnail is drawn in memory and dropped once it is sent, and an original
 * is streamed straight from the store to the connection.
 *
 * Only what Android shows this app is ever read: with the person's partial
 * "selected photos" access, the store answers with those photos and no
 * others, so a listing has nothing else to leave out and a fetch of any other
 * id finds nothing. ACCESS_MEDIA_LOCATION is not asked for, so on Android 10
 * and later Android blanks the location in an original's EXIF before this app
 * reads it; Android 9 hands the file over as it is.
 */
object PhotoStore {
    private const val TAG = "RimeRemote"

    /**
     * What Android lets this app read now. On Android 14 and later the
     * person may allow only the photos they picked, which Android grants as
     * READ_MEDIA_VISUAL_USER_SELECTED without READ_MEDIA_IMAGES.
     */
    fun access(context: Context): Photos.Access = when {
        Build.VERSION.SDK_INT >= 33 && granted(context, Manifest.permission.READ_MEDIA_IMAGES) -> Photos.Access.FULL
        Build.VERSION.SDK_INT >= 34 && granted(context, Manifest.permission.READ_MEDIA_VISUAL_USER_SELECTED) -> Photos.Access.PARTIAL
        Build.VERSION.SDK_INT < 33 && granted(context, Manifest.permission.READ_EXTERNAL_STORAGE) -> Photos.Access.FULL
        else -> Photos.Access.NONE
    }

    /**
     * What the screen asks Android for. On Android 14 and later both, so the
     * prompt offers "Select photos" beside "Allow all"; asking again later
     * is how the person picks more.
     */
    fun permissions(): Array<String> = when {
        Build.VERSION.SDK_INT >= 34 -> arrayOf(Manifest.permission.READ_MEDIA_IMAGES, Manifest.permission.READ_MEDIA_VISUAL_USER_SELECTED)
        Build.VERSION.SDK_INT >= 33 -> arrayOf(Manifest.permission.READ_MEDIA_IMAGES)
        else -> arrayOf(Manifest.permission.READ_EXTERNAL_STORAGE)
    }

    private fun granted(context: Context, permission: String): Boolean =
        ContextCompat.checkSelfPermission(context, permission) == PackageManager.PERMISSION_GRANTED

    /** Every external volume's images on Android 10 and later; the primary one before. */
    private val collection: Uri
        get() = if (Build.VERSION.SDK_INT >= 29) {
            MediaStore.Images.Media.getContentUri(MediaStore.VOLUME_EXTERNAL)
        } else {
            MediaStore.Images.Media.EXTERNAL_CONTENT_URI
        }

    // DATE_TAKEN and ORIENTATION moved to MediaColumns in Android 10; the
    // column names did not change, and the older constants reach back to 9.
    @Suppress("DEPRECATION")
    private val PROJECTION = arrayOf(
        MediaStore.Images.Media._ID,
        MediaStore.Images.Media.DISPLAY_NAME,
        MediaStore.Images.Media.DATE_TAKEN,
        MediaStore.Images.Media.DATE_ADDED,
        MediaStore.Images.Media.WIDTH,
        MediaStore.Images.Media.HEIGHT,
        MediaStore.Images.Media.SIZE,
        MediaStore.Images.Media.MIME_TYPE,
        MediaStore.Images.Media.ORIENTATION,
    )

    /**
     * One page of the listing (see [Photos.page]), each photo with a
     * thumbnail when one could be drawn. A photo whose thumbnail fails is
     * still listed, without one.
     */
    fun listing(context: Context, before: Long?, limit: Int): List<Photos.Photo> {
        val turns = HashMap<Long, Int>()
        val page = rows(context, null, null, turns) { Photos.page(it, before, limit) }
        return page.map { p ->
            val thumb = runCatching { thumbnail(context, p, turns[p.id] ?: 0) }
                .onFailure { Log.i(TAG, "a thumbnail was not drawn: ${it.javaClass.simpleName}") }
                .getOrNull()
            p.copy(thumb = thumb)
        }
    }

    /** A photo Android shows this app, ready to send: its name, its length and where to read it. */
    class Original(val id: Long, val name: String, val len: Long, val uri: Uri) {
        override fun toString(): String = "Original(id=$id, len=$len)"
    }

    /**
     * The photo with [id], or null when there is none this app may read (no
     * such photo, not an image, or not one of the photos the person picked).
     * The length is the open file's, not the store's SIZE column, which can
     * lag a file still being written; the upload refuses any other length.
     */
    fun original(context: Context, id: Long): Original? {
        val row = rows(context, "${MediaStore.Images.Media._ID} = ?", arrayOf(id.toString()), HashMap()) { it.firstOrNull() }
            ?: return null
        val photo = Photos.photo(row) ?: return null
        val uri = ContentUris.withAppendedId(collection, id)
        val len = context.contentResolver.openFileDescriptor(uri, "r")?.use { it.statSize } ?: return null
        return Original(id, photo.name, len, uri)
    }

    /** The original's bytes, as Android gives them to this app. */
    fun open(context: Context, original: Original): InputStream =
        context.contentResolver.openInputStream(original.uri) ?: throw java.io.FileNotFoundException("the photo could not be opened")

    private fun <T> rows(
        context: Context,
        selection: String?,
        args: Array<String>?,
        turns: MutableMap<Long, Int>,
        use: (Sequence<Photos.Row>) -> T,
    ): T {
        // Newest added first, so the scan cap leaves out the oldest; the page
        // itself is ordered by when each photo was taken, in [Photos.page].
        val cursor = context.contentResolver.query(
            collection,
            PROJECTION,
            selection,
            args,
            "${MediaStore.Images.Media.DATE_ADDED} DESC",
        ) ?: return use(emptySequence())
        return cursor.use { c ->
            val seq = sequence {
                while (c.moveToNext()) {
                    val id = c.getLong(0)
                    if (!c.isNull(8) && c.getInt(8) != 0) turns[id] = c.getInt(8)
                    yield(
                        Photos.Row(
                            id = id,
                            name = c.getString(1),
                            dateTakenMs = if (c.isNull(2)) null else c.getLong(2),
                            dateAddedS = if (c.isNull(3)) null else c.getLong(3),
                            width = if (c.isNull(4)) null else c.getInt(4),
                            height = if (c.isNull(5)) null else c.getInt(5),
                            size = if (c.isNull(6)) null else c.getLong(6),
                            mime = c.getString(7),
                        ),
                    )
                }
            }
            use(seq)
        }
    }

    /**
     * A thumbnail of [photo], base64url, drawn from its pixels and encoded
     * here (see [Photos.thumb]): nothing of the original file's metadata
     * survives a decode and a re-encode, and the stripper checks it again.
     * On Android 10 and later the store's own thumbnail is the source (fast,
     * already turned upright); before, the original decoded at a fraction of
     * its size and turned by its [orientation].
     */
    private fun thumbnail(context: Context, photo: Photos.Photo, orientation: Int): String? {
        val uri = ContentUris.withAppendedId(collection, photo.id)
        val source = if (Build.VERSION.SDK_INT >= 29) {
            context.contentResolver.loadThumbnail(uri, Size(Photos.THUMB_SIDE, Photos.THUMB_SIDE), null)
        } else {
            decodeSmall(context, uri, orientation)
        } ?: return null
        val opaque = opaque(source)
        try {
            return Photos.thumb { side, quality ->
                // The store's thumbnail may be larger than asked for: always scaled here.
                val (w, h) = Photos.targetSize(opaque.width, opaque.height, side) ?: return@thumb null
                val scaled = if (w == opaque.width && h == opaque.height) opaque else Bitmap.createScaledBitmap(opaque, w, h, true)
                try {
                    val out = ByteArrayOutputStream()
                    if (!scaled.compress(Bitmap.CompressFormat.JPEG, quality, out)) null else out.toByteArray()
                } finally {
                    if (scaled !== opaque) scaled.recycle()
                }
            }
        } finally {
            if (opaque !== source) opaque.recycle()
            source.recycle()
        }
    }

    /** A picture with transparency drawn on white, since a JPEG has none and would show it black. */
    private fun opaque(bitmap: Bitmap): Bitmap {
        if (!bitmap.hasAlpha()) return bitmap
        val out = Bitmap.createBitmap(bitmap.width, bitmap.height, Bitmap.Config.ARGB_8888)
        val canvas = Canvas(out)
        canvas.drawColor(Color.WHITE)
        canvas.drawBitmap(bitmap, 0f, 0f, null)
        return out
    }

    /** Android 9: the original, decoded at the largest power-of-two fraction still at least a thumbnail's size. */
    private fun decodeSmall(context: Context, uri: Uri, orientation: Int): Bitmap? {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        context.contentResolver.openInputStream(uri)?.use { BitmapFactory.decodeStream(it, null, bounds) }
        val long = maxOf(bounds.outWidth, bounds.outHeight)
        if (long <= 0) return null
        var sample = 1
        while (long / (sample * 2) >= Photos.THUMB_SIDE) sample *= 2
        val decoded = context.contentResolver.openInputStream(uri)?.use {
            BitmapFactory.decodeStream(it, null, BitmapFactory.Options().apply { inSampleSize = sample })
        } ?: return null
        if (orientation == 0) return decoded
        val turned = Bitmap.createBitmap(decoded, 0, 0, decoded.width, decoded.height, Matrix().apply { postRotate(orientation.toFloat()) }, true)
        if (turned !== decoded) decoded.recycle()
        return turned
    }
}
