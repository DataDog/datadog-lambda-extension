package example;

import com.amazonaws.services.lambda.runtime.Context;
import com.amazonaws.services.lambda.runtime.RequestHandler;
import com.fasterxml.jackson.databind.ObjectMapper;
import okhttp3.OkHttpClient;
import okhttp3.Request;
import okhttp3.Response;
import java.util.HashMap;
import java.util.Map;
import java.util.concurrent.TimeUnit;

/**
 * Lambda handler designed to test SnapStart timestamp adjustment.
 *
 * This handler uses OkHttp to make HTTP requests during class initialization.
 * OkHttp is auto-instrumented by dd-trace-java, creating spans that get
 * captured in the SnapStart snapshot. When the snapshot is restored after
 * a delay, these spans will have stale timestamps unless the extension
 * adjusts them.
 */
public class Handler implements RequestHandler<Map<String, Object>, Map<String, Object>> {

    private static final ObjectMapper objectMapper = new ObjectMapper();

    // Initialize OkHttp client during class loading (captured in snapshot)
    private static final OkHttpClient httpClient = new OkHttpClient.Builder()
        .connectTimeout(10, TimeUnit.SECONDS)
        .readTimeout(10, TimeUnit.SECONDS)
        .build();

    // Track whether we made the init request
    private static boolean initRequestMade = false;
    private static int initStatusCode = 0;

    // Make an HTTP request during static initialization to create a span
    // This span's timestamp will be from snapshot creation time
    static {
        try {
            System.out.println("Making HTTP request during static initialization...");
            Request request = new Request.Builder()
                .url("https://httpbin.org/get")
                .build();
            try (Response response = httpClient.newCall(request).execute()) {
                initStatusCode = response.code();
                initRequestMade = (initStatusCode == 200);
                System.out.println("Init HTTP request completed with status: " + initStatusCode);
            }
        } catch (Exception e) {
            System.out.println("Init HTTP request failed: " + e.getMessage());
            // Continue even if request fails - the span should still be created
        }
    }

    @Override
    public Map<String, Object> handleRequest(Map<String, Object> event, Context context) {
        context.getLogger().log("SnapStart timing test handler invoked");
        context.getLogger().log("Init request was made during class loading: " + initRequestMade);
        context.getLogger().log("Init request status code: " + initStatusCode);

        // Make another HTTP request during invocation for comparison
        boolean invokeRequestSuccess = false;
        int invokeStatusCode = 0;
        try {
            Request request = new Request.Builder()
                .url("https://httpbin.org/get")
                .build();
            try (Response response = httpClient.newCall(request).execute()) {
                invokeStatusCode = response.code();
                invokeRequestSuccess = (invokeStatusCode == 200);
                context.getLogger().log("Invoke HTTP request completed with status: " + invokeStatusCode);
            }
        } catch (Exception e) {
            context.getLogger().log("Invoke HTTP request failed: " + e.getMessage());
        }

        Map<String, Object> body = new HashMap<>();
        body.put("message", "Success");
        body.put("requestId", context.getAwsRequestId());
        body.put("initRequestMade", initRequestMade);
        body.put("initStatusCode", initStatusCode);
        body.put("invokeRequestSuccess", invokeRequestSuccess);
        body.put("invokeStatusCode", invokeStatusCode);

        Map<String, Object> response = new HashMap<>();
        response.put("statusCode", 200);
        try {
            response.put("body", objectMapper.writeValueAsString(body));
        } catch (Exception e) {
            response.put("body", "{}");
        }

        return response;
    }
}
