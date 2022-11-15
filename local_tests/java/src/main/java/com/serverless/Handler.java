package com.serverless;

import com.amazonaws.services.lambda.runtime.events.APIGatewayProxyRequestEvent;
import com.amazonaws.services.lambda.runtime.events.APIGatewayV2ProxyResponseEvent;
import com.amazonaws.services.lambda.runtime.Context;
import com.amazonaws.services.lambda.runtime.RequestHandler;

import java.io.IOException;
import java.io.InputStream;
import java.io.StringWriter;
import java.net.HttpURLConnection;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.util.Collections;
import java.util.Map;
import java.util.HashMap;

import org.apache.commons.io.IOUtils;

public class Handler implements RequestHandler<APIGatewayProxyRequestEvent, APIGatewayV2ProxyResponseEvent> {

	@Override
	public APIGatewayV2ProxyResponseEvent handleRequest(APIGatewayProxyRequestEvent input, Context context) {
		doRequest();
		Map<String, String> headers = new HashMap<>();
        headers.put("Content-Type", "application/json");
        APIGatewayV2ProxyResponseEvent response = new APIGatewayV2ProxyResponseEvent();
        response.setBody("success");
        response.setHeaders(headers);
        response.setStatusCode(200);
        return response;
	}

	public void doRequest() {
		final String urlToFetch = System.getenv("URL_TO_FETCH");
		if(null != urlToFetch) {
			try {
				HttpURLConnection connection = null;
				URL url = new URL(urlToFetch);
				connection = (HttpURLConnection) url.openConnection();
				InputStream responseStream;
				responseStream = connection.getInputStream();
				StringWriter sw = new StringWriter();
				IOUtils.copy(responseStream, sw, StandardCharsets.UTF_8);
				System.out.println("Got an item: " + sw.toString());
				responseStream.close();
			} catch (IOException e) {
				e.printStackTrace();
			}
		}
	}
}
