import os
import urllib.request
from threading import Thread
from flask import Flask, Response

app = Flask(__name__)


@app.route("/")
def home():
    return Response("{\"msg\":\"Hello!\"}", status=200, mimetype='application/json')


app.run(host="0.0.0.0", port=int(os.environ.get("PORT", 8080)))
