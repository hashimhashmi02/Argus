#!/usr/bin/env node
"use strict";

const { runConsole, fail } = require("../lib/run");

runConsole("argus-engine.exe").catch(fail);
