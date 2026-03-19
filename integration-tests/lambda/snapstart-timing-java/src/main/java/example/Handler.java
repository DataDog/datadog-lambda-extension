package example;

import com.amazonaws.services.lambda.runtime.Context;
import com.amazonaws.services.lambda.runtime.RequestHandler;
import com.fasterxml.jackson.databind.ObjectMapper;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.net.URI;
import java.time.Duration;
import java.util.HashMap;
import java.util.Map;

/**
 * Lambda handler designed to test SnapStart timestamp adjustment.
 *
 * This handler makes HTTP requests during class initialization (static block)
 * which creates tracer spans that get captured in the SnapStart snapshot.
 * When the snapshot is restored after a delay, these spans will have stale
 * timestamps unless the extension adjusts them.
 */
public class Handler implements RequestHandler<Map<String, Object>, Map<String, Object>> {

    private static final ObjectMapper objectMapper = new ObjectMapper();

    // Initialize HTTP client during class loading (captured in snapshot)
    private static final HttpClient httpClient = HttpClient.newBuilder()
        .connectTimeout(Duration.ofSeconds(10))
        .build();

    // Track whether we made the init request
    private static boolean initRequestMade = false;

    // Make an HTTP request during static initialization to create a span
    // This span's timestamp will be from snapshot creation time
    static {
        try {
            HttpRequest request = HttpRequest.newBuilder()
                .uri(URI.create("https://httpbin.org/get"))
                .timeout(Duration.ofSeconds(5))
                .GET()
                .build();
            HttpResponse<String> response = httpClient.send(request, HttpResponse.BodyHandlers.ofString());
            initRequestMade = (response.statusCode() == 200);
            System.out.println("Init HTTP request completed with status: " + response.statusCode());
        } catch (Exception e) {
            System.out.println("Init HTTP request failed: " + e.getMessage());
            // Continue even if request fails - the span should still be created
        }
    }

    @Override
    public Map<String, Object> handleRequest(Map<String, Object> event, Context context) {
        context.getLogger().log("SnapStart timing test handler invoked");
        context.getLogger().log("Init request was made during class loading: " + initRequestMade);

        // Make another HTTP request during invocation for comparison
        boolean invokeRequestSuccess = false;
        try {
            HttpRequest request = HttpRequest.newBuilder()
                .uri(URI.create("https://httpbin.org/get"))
                .timeout(Duration.ofSeconds(5))
                .GET()
                .build();
            HttpResponse<String> response = httpClient.send(request, HttpResponse.BodyHandlers.ofString());
            invokeRequestSuccess = (response.statusCode() == 200);
            context.getLogger().log("Invoke HTTP request completed with status: " + response.statusCode());
        } catch (Exception e) {
            context.getLogger().log("Invoke HTTP request failed: " + e.getMessage());
        }

        Map<String, Object> body = new HashMap<>();
        body.put("message", "Success");
        body.put("requestId", context.getAwsRequestId());
        body.put("initRequestMade", initRequestMade);
        body.put("invokeRequestSuccess", invokeRequestSuccess);

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
