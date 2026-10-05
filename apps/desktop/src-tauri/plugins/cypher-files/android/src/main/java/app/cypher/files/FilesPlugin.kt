package app.cypher.files

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.ContentValues
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import android.provider.OpenableColumns
import android.webkit.MimeTypeMap
import androidx.annotation.RequiresApi
import androidx.core.content.FileProvider
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File
import java.io.IOException

private const val FOLDER = "Cypher"
private const val OCTET_STREAM = "application/octet-stream"

@InvokeArg
class PublishArgs {
    lateinit var path: String
    lateinit var name: String
    var mime: String = ""
}

@InvokeArg
class UriArgs {
    lateinit var uri: String
}

/** Serves files kept in app storage on devices without MediaStore downloads. */
class CypherFileProvider : FileProvider()

@TauriPlugin
class FilesPlugin(private val activity: Activity) : Plugin(activity) {
    private val resolver get() = activity.contentResolver

    /** Moves a received file into Downloads/Cypher and answers its URI. */
    @Command
    fun publish(invoke: Invoke) {
        val args = invoke.parseArgs(PublishArgs::class.java)
        background(invoke) {
            val source = File(args.path)
            val mime = mimeOf(args.mime, args.name)
            val uri = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                toDownloads(source, args.name, mime)
            } else {
                // Before Android 10 shared Downloads needs a storage
                // permission; the file stays in app storage instead.
                FileProvider.getUriForFile(activity, "${activity.packageName}.cypher.files", source)
            }
            JSObject().put("uri", uri.toString())
        }
    }

    /** Opens a file with the app the user prefers for its type. */
    @Command
    fun open(invoke: Invoke) {
        val args = invoke.parseArgs(UriArgs::class.java)
        try {
            val uri = Uri.parse(args.uri)
            val view = Intent(Intent.ACTION_VIEW)
                .setDataAndType(uri, resolver.getType(uri) ?: OCTET_STREAM)
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
            activity.startActivity(view)
            invoke.resolve()
        } catch (e: ActivityNotFoundException) {
            invoke.reject("no_app")
        } catch (e: Exception) {
            invoke.reject(e.message ?: "cannot open the file")
        }
    }

    /** Copies a picked `content://` file into the app so it can be sent. */
    @Command
    fun stage(invoke: Invoke) {
        val args = invoke.parseArgs(UriArgs::class.java)
        background(invoke) {
            val uri = Uri.parse(args.uri)
            val name = displayName(uri) ?: uri.lastPathSegment ?: "file"
            val dir = File(activity.cacheDir, "staged").apply { mkdirs() }
            val target = File.createTempFile("pick-", ".tmp", dir)
            try {
                val input = resolver.openInputStream(uri) ?: throw IOException("cannot read the file")
                input.use { from -> target.outputStream().use { from.copyTo(it) } }
            } catch (e: Exception) {
                target.delete()
                throw e
            }
            JSObject()
                .put("path", target.absolutePath)
                .put("name", name)
                .put("mime", resolver.getType(uri) ?: mimeOf("", name))
        }
    }

    @RequiresApi(Build.VERSION_CODES.Q)
    private fun toDownloads(source: File, name: String, mime: String): Uri {
        val values = ContentValues().apply {
            put(MediaStore.Downloads.DISPLAY_NAME, name)
            put(MediaStore.Downloads.MIME_TYPE, mime)
            put(MediaStore.Downloads.RELATIVE_PATH, "${Environment.DIRECTORY_DOWNLOADS}/$FOLDER")
            put(MediaStore.Downloads.IS_PENDING, 1)
        }
        val uri = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
            ?: throw IOException("Downloads refused the file")
        try {
            val out = resolver.openOutputStream(uri) ?: throw IOException("cannot write to Downloads")
            out.use { to -> source.inputStream().use { it.copyTo(to) } }
            resolver.update(uri, ContentValues().apply { put(MediaStore.Downloads.IS_PENDING, 0) }, null, null)
        } catch (e: Exception) {
            resolver.delete(uri, null, null)
            throw e
        }
        source.delete()
        return uri
    }

    private fun displayName(uri: Uri): String? =
        resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { row ->
            if (row.moveToFirst()) row.getString(0) else null
        }

    /** The sender's type, or one guessed from the name when it says nothing. */
    private fun mimeOf(given: String, name: String): String {
        if (given.isNotBlank() && given != OCTET_STREAM) return given
        val ext = name.substringAfterLast('.', "").lowercase()
        return MimeTypeMap.getSingleton().getMimeTypeFromExtension(ext) ?: OCTET_STREAM
    }

    /** Runs file IO off the main thread and answers with its result. */
    private fun background(invoke: Invoke, work: () -> JSObject) {
        Thread {
            try {
                invoke.resolve(work())
            } catch (e: Exception) {
                invoke.reject(e.message ?: e.javaClass.simpleName)
            }
        }.start()
    }
}
