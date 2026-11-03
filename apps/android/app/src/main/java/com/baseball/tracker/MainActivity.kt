package com.baseball.tracker

import android.annotation.SuppressLint
import android.app.AlertDialog
import android.os.Bundle
import android.view.WindowManager
import android.webkit.WebResourceError
import android.webkit.WebResourceRequest
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.EditText
import android.widget.FrameLayout
import androidx.activity.ComponentActivity
import androidx.activity.OnBackPressedCallback
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import org.json.JSONObject
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetAddress
import kotlin.concurrent.thread

/**
 * The tracker's pages in a WebView, with the screen kept on. The tracker is
 * found by its discovery broadcast, or at the address last used, or one
 * the user types when nothing answers. The page is padded clear of the
 * status bar, navigation bar, cutout and keyboard, which the window
 * otherwise draws under.
 */
class MainActivity : ComponentActivity() {
    private lateinit var web: WebView
    private val prefs by lazy { getSharedPreferences("tracker", MODE_PRIVATE) }

    @SuppressLint("SetJavaScriptEnabled")
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        web = WebView(this)
        web.settings.javaScriptEnabled = true
        web.settings.domStorageEnabled = true
        web.settings.mediaPlaybackRequiresUserGesture = false
        web.setBackgroundColor(0xff09090b.toInt())
        web.webViewClient = object : WebViewClient() {
            override fun onReceivedError(view: WebView, request: WebResourceRequest, error: WebResourceError) {
                if (request.isForMainFrame) find()
            }
        }
        setContentView(frame(web))
        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() {
                if (web.canGoBack()) web.goBack() else finish()
            }
        })
        val saved = prefs.getString(URL, null)
        if (saved != null) web.loadUrl(saved) else find()
    }

    private fun frame(web: WebView): FrameLayout {
        val frame = FrameLayout(this)
        frame.setBackgroundColor(0xff09090b.toInt())
        frame.addView(web)
        ViewCompat.setOnApplyWindowInsetsListener(frame) { view, insets ->
            val bars = insets.getInsets(
                WindowInsetsCompat.Type.systemBars() or
                    WindowInsetsCompat.Type.displayCutout() or
                    WindowInsetsCompat.Type.ime()
            )
            view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
            WindowInsetsCompat.CONSUMED
        }
        return frame
    }

    /** Looks for a tracker on the network; asks for an address when none answers. */
    private fun find() {
        thread {
            val url = discover()
            runOnUiThread { if (url != null) open(url) else ask() }
        }
    }

    private fun discover(): String? = try {
        DatagramSocket().use { socket ->
            socket.broadcast = true
            socket.soTimeout = 1500
            val request = """{"type":"discover","version":1}""".toByteArray()
            socket.send(DatagramPacket(request, request.size, InetAddress.getByName("255.255.255.255"), 7881))
            val buffer = ByteArray(1024)
            val packet = DatagramPacket(buffer, buffer.size)
            socket.receive(packet)
            val reply = JSONObject(String(packet.data, 0, packet.length))
            if (reply.optString("type") == "tracker") {
                "http://${packet.address.hostAddress}:${reply.optInt("port", 7880)}/"
            } else {
                null
            }
        }
    } catch (e: Exception) {
        null
    }

    private fun open(url: String) {
        prefs.edit().putString(URL, url).apply()
        web.loadUrl(url)
    }

    private fun ask() {
        val field = EditText(this)
        field.hint = "192.168.1.20:7880"
        field.setText(prefs.getString(URL, "")?.removePrefix("http://")?.trimEnd('/'))
        AlertDialog.Builder(this)
            .setTitle("Tracker address")
            .setMessage("No tracker answered on this network.")
            .setView(field)
            .setPositiveButton("Open") { _, _ ->
                val typed = field.text.toString().trim()
                if (typed.isNotEmpty()) open(if (typed.contains("://")) typed else "http://$typed/")
            }
            .setNeutralButton("Search again") { _, _ -> find() }
            .show()
    }

    companion object {
        private const val URL = "url"
    }
}
