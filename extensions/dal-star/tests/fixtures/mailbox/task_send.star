s = ctx.scope(limit = 1)
t = s.tools.read(path = "x")
t.send("hello", "aside")
