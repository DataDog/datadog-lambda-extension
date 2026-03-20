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
 * This handler uses OkHttp to make HTTP requests during invocation.
 * OkHttp is auto-instrumented by dd-trace-java, creating spans.
 * We verify that spans created during invocation have correct timestamps.
 */
public class Handler implements RequestHandler<Map<String, Object>, Map<String, Object>> {

    private static final ObjectMapper objectMapper = new ObjectMapper();

    // Initialize OkHttp client during class loading
    private static final OkHttpClient httpClient = new OkHttpClient.Builder()
        .connectTimeout(10, TimeUnit.SECONDS)
        .readTimeout(10, TimeUnit.SECONDS)
        .build();

    @Override
    public Map<String, Object> handleRequest(Map<String, Object> event, Context context) {
        context.getLogger().log("SnapStart timing test handler invoked");

        // Make HTTP request during invocation - this creates an OkHttp span
        boolean invokeRequestSuccess = false;
        int invokeStatusCode = 0;
        try {
            Request request = new Request.Builder()
                .url("https://httpbin.org/get")
                .build();
            try (Response response = httpClient.newCall(request).execute()) {
                invokeStatusCode = response.code();
                invokeRequestSuccess = (invokeStatusCode == 200);
                context.getLogger().log("HTTP request completed with status: " + invokeStatusCode);
            }
        } catch (Exception e) {
            context.getLogger().log("HTTP request failed: " + e.getMessage());
        }

        Map<String, Object> body = new HashMap<>();
        body.put("message", "Success");
        body.put("requestId", context.getAwsRequestId());
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
