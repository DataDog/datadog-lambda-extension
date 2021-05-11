#!/usr/bin/env node

const express = require('express');
const fetch = require('node-fetch');
const bodyParser = require('body-parser');
const protobuf = require('protobufjs');

const BASE_URL = `http://${process.env.AWS_LAMBDA_RUNTIME_API}/2020-01-01/extension`;
const SHUTDOWN_EVENT = 'SHUTDOWN';

const handleShutdown = (app) => {
    app.close();
    process.exit(0);
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
            'Lambda-Extension-Name': 'man-in-the-middle',
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
        limit: '100kb',
        type: 'application/x-protobuf'
    };

    app.use(bodyParser.raw(options));

    process.on('SIGINT', () => handleShutdown(app));
    process.on('SIGTERM', () => handleShutdown(app));

    const extensionId = await register();

    const port = 3333;

    app.get('/*', (req, res) => {
        //GET catch all
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

    app.post('/*', (req, res) => {
        //POST catch all
        res.sendStatus(200);
    });

    app.listen(port, () => {
        console.log(`Man-in-the-middle extension started on port : ${port}`);
    })

    while (true) {
        const event = await next(extensionId);
        if(event.eventType === SHUTDOWN_EVENT) {
            handleShutdown(app);
            break;
        } else {
            throw new Error('Unexpected event');
        }
    }
})();
