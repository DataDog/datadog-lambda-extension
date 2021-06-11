#!/usr/bin/env node

const express = require('express');
const fetch = require('node-fetch');
const bodyParser = require('body-parser');
const protobuf = require('protobufjs');

const BASE_URL = `http://${process.env.AWS_LAMBDA_RUNTIME_API}/2020-01-01/extension`;
const SHUTDOWN_EVENT = 'SHUTDOWN';

const handleShutdown = async () => {
    console.log('SHUTDOWN received in recorder');
    //making sure that the server won't be closed when the extension will send data
    await new Promise(r => setTimeout(r, 1000));
    console.log('SHUTDOWN end');
}

async function register() {
    const res = await fetch(`${BASE_URL}/register`, {
        method: 'post',
        body: JSON.stringify({
            'events': [
                SHUTDOWN_EVENT
            ],
        }),
        headers: {
            'Content-Type': 'application/json',
            'Lambda-Extension-Name': 'a_recorder',
        }
    });

    if (!res.ok) {
        console.error('register failed', await res.text());
    }
    return res.headers.get('lambda-extension-identifier');
}

async function next(extensionId) {
    const res = await fetch(`${BASE_URL}/event/next`, {
        method: 'get',
        headers: {
            'Content-Type': 'application/json',
            'Lambda-Extension-Identifier': extensionId,
        }
    });

    if (!res.ok) {
        console.error('next failed', await res.text());
        return null;
    }

    return await res.json();
}

(async function main() {

    const app = express();
    const options = {
        inflate: true,
        limit: '300kb',
        type: 'application/x-protobuf'
    };

    app.use(bodyParser.raw(options));
    app.use(bodyParser.json());

    const extensionId = await register();

    const port = 3333;

    app.get('/*', (req, res) => {
        console.log("GET", req.url);
        res.sendStatus(200);
    });

    app.post('/api/beta/sketches*', async (req, res) => {
        const root = await protobuf.load('/opt/extensions/src/agent_payload.proto');
        const SketchPayload = root.lookupType('datadog.agentpayload.SketchPayload');
        const obj = SketchPayload.decode(req.body);
        for(let i = 0; i < obj.sketches.length; ++i) {
            console.log("[sketch]", JSON.stringify(obj.sketches[i]));
        }
        res.sendStatus(200);
    });


    app.post('/v1/input', async (req, res) => {
        if(JSON.stringify(req.body) !== '{}') { // to avoid printing empty logs due to the connectivity test
            for(let i = 0; i < req.body.length; ++i) {
                //sort tags to avoid flakky tests
                const originalTags = req.body[i].ddtags;
                const sortedTags = originalTags.split(",");
                sortedTags.sort();
                // reset tags once sorted
                req.body[i].ddtags = sortedTags.join(",")
                const logString = JSON.stringify(req.body[i]);
                if(logString.indexOf("[sketch]") === -1 && logString.indexOf("[log]") === -1) { // avoid inception
                    const tags = logString
                    console.log("[log]", logString);
                }
            }
        }
        res.sendStatus(200);
    });

    app.post('/*', (req, res) => {
        console.log("POST", req.url);
        res.sendStatus(200);
    });

    app.listen(port);

    process.on('SIGINT', async () => await handleShutdown());
    process.on('SIGTERM', async () => await handleShutdown());

    while (true) {
        const event = await next(extensionId);
        if(event.eventType === SHUTDOWN_EVENT) {
            await handleShutdown();
            break;
        } else {
            await handleShutdown();
            throw new Error('Unexpected event');
        }
    }
})();
