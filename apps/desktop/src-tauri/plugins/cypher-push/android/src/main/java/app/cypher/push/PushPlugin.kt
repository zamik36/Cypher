package app.cypher.push

import android.app.Activity
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import org.unifiedpush.android.connector.FailedReason
import org.unifiedpush.android.connector.PushService
import org.unifiedpush.android.connector.UnifiedPush
import org.unifiedpush.android.connector.data.PushEndpoint
import org.unifiedpush.android.connector.data.PushMessage
import java.util.Locale

private const val CHANNEL = "messages"
private const val NOTIFICATION_ID = 1

@InvokeArg
class SubscribeArgs {
    lateinit var vapid: String
}

/** Registration answers arrive in the service; the waiting call gets them. */
internal object Registration {
    private var waiting: Invoke? = null

    @Synchronized
    fun await(invoke: Invoke) {
        waiting?.resolve(JSObject().put("status", "failed"))
        waiting = invoke
    }

    @Synchronized
    fun answer(result: JSObject) {
        waiting?.resolve(result)
        waiting = null
    }
}

@TauriPlugin
class PushPlugin(private val activity: Activity) : Plugin(activity) {
    @Command
    fun subscribe(invoke: Invoke) {
        val args = invoke.parseArgs(SubscribeArgs::class.java)
        UnifiedPush.tryUseCurrentOrDefaultDistributor(activity) { found ->
            if (!found) {
                invoke.resolve(JSObject().put("status", "no_distributor"))
                return@tryUseCurrentOrDefaultDistributor
            }
            Registration.await(invoke)
            UnifiedPush.register(activity.applicationContext, vapid = args.vapid)
        }
    }

    @Command
    fun unsubscribe(invoke: Invoke) {
        UnifiedPush.unregister(activity.applicationContext)
        invoke.resolve(JSObject())
    }
}

/**
 * The distributor's side: a new endpoint for the server, or a signal that
 * the inbox has news. A signal carries nothing else, so the notification
 * says only that something arrived; opening it starts the app, which
 * fetches and decrypts the rest itself.
 */
class CypherPushService : PushService() {
    override fun onNewEndpoint(endpoint: PushEndpoint, instance: String) {
        val keys = endpoint.pubKeySet
        if (keys == null) {
            Registration.answer(JSObject().put("status", "failed"))
            return
        }
        Registration.answer(
            JSObject()
                .put("status", "ok")
                .put("endpoint", endpoint.url)
                .put("p256dh", keys.pubKey)
                .put("auth", keys.auth),
        )
    }

    override fun onRegistrationFailed(reason: FailedReason, instance: String) {
        Registration.answer(JSObject().put("status", "failed").put("reason", reason.name))
    }

    override fun onUnregistered(instance: String) {}

    override fun onMessage(message: PushMessage, instance: String) {
        if (!message.decrypted) return
        notifyNews(this)
    }
}

private fun notifyNews(context: Context) {
    val manager = NotificationManagerCompat.from(context)
    if (!manager.areNotificationsEnabled()) return
    val ru = Locale.getDefault().language == "ru"
    val title = if (ru) "Шифр" else "Cypher"
    val text = if (ru) "Новое сообщение" else "New message"
    manager.createNotificationChannel(
        NotificationChannel(CHANNEL, if (ru) "Сообщения" else "Messages", NotificationManager.IMPORTANCE_HIGH),
    )
    val launch = context.packageManager.getLaunchIntentForPackage(context.packageName)
        ?.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
    val open = launch?.let {
        PendingIntent.getActivity(context, 0, it, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
    }
    val notification = NotificationCompat.Builder(context, CHANNEL)
        .setSmallIcon(context.applicationInfo.icon)
        .setContentTitle(title)
        .setContentText(text)
        .setPriority(NotificationCompat.PRIORITY_HIGH)
        .setCategory(NotificationCompat.CATEGORY_MESSAGE)
        .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
        .setAutoCancel(true)
        .setContentIntent(open)
        .build()
    try {
        manager.notify(NOTIFICATION_ID, notification)
    } catch (_: SecurityException) {
        // Notifications were turned off between the check and now.
    }
}
