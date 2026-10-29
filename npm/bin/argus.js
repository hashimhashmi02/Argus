#!/usr/bin/env node
"use strict";

const { runWindow, fail } = require("../lib/run");

runWindow("Argus.exe").catch(fail);
