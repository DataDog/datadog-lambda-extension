import com.amazonaws.services.lambda.runtime.Context;
import com.amazonaws.services.lambda.runtime.RequestHandler;
import org.apache.logging.log4j.LogManager;
import org.apache.logging.log4j.Logger;

import java.util.Map;
import java.util.HashMap;

public class Handler implements RequestHandler<Map<String, Object>, Map<String, Object>> {
    private static final Logger logger = LogManager.getLogger(Handler.class);

    @Override
    public Map<String, Object> handleRequest(Map<String, Object> event, Context context) {
        logger.info("Hello world!");

        Map<String, Object> response = new HashMap<>();
        response.put("statusCode", 200);
        return response;
    }
}
