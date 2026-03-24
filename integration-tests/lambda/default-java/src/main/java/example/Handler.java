package example;

import com.amazonaws.services.lambda.runtime.Context;
import com.amazonaws.services.lambda.runtime.RequestHandler;
import com.fasterxml.jackson.databind.ObjectMapper;
import java.util.HashMap;
import java.util.Map;

public class Handler implements RequestHandler<Map<String, Object>, Map<String, Object>> {

    private static final ObjectMapper objectMapper = new ObjectMapper();

    @Override
    public Map<String, Object> handleRequest(Map<String, Object> event, Context context) {
        context.getLogger().log("Hello world!");

        String sleepMsStr = System.getenv("SLEEP_MS");
        if (sleepMsStr != null && !sleepMsStr.isEmpty()) {
            try {
                int sleepMs = Integer.parseInt(sleepMsStr);
                if (sleepMs > 0) {
                    context.getLogger().log("Sleeping for " + sleepMs + "ms");
                    Thread.sleep(sleepMs);
                }
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            } catch (NumberFormatException e) {
                context.getLogger().log("Invalid SLEEP_MS value: " + sleepMsStr);
            }
        }

        Map<String, Object> body = new HashMap<>();
        body.put("message", "Success");
        body.put("requestId", context.getAwsRequestId());

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
