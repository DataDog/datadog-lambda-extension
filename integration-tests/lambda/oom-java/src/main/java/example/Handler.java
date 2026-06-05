package example;

import com.amazonaws.services.lambda.runtime.Context;
import com.amazonaws.services.lambda.runtime.RequestHandler;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;

/**
 * OOM reproducer for Java. Allocates and retains 10 MB byte arrays in a list
 * until the JVM throws java.lang.OutOfMemoryError: Java heap space.
 * Bottlecap's runtime-specific log-line detection matches
 * "java.lang.OutOfMemoryError".
 */
public class Handler implements RequestHandler<Map<String, Object>, Map<String, Object>> {

    @Override
    public Map<String, Object> handleRequest(Map<String, Object> event, Context context) {
        List<byte[]> data = new ArrayList<>();
        while (true) {
            data.add(new byte[10 * 1024 * 1024]);
        }
    }
}
