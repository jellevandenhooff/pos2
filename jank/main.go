package main

import (
	"encoding/json"
	"fmt"
	"log"
	"maps"
	"net/http"
	"slices"
	"time"
)

func main() {
	started := fmt.Sprint(time.Now().Unix())

	mux := http.NewServeMux()

	mux.HandleFunc("/endpoint", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Add("Access-Control-Allow-Origin", "*")

		s := "endpoint got: " + r.Header.Get("Cookie")
		b, err := json.Marshal(s)
		if err != nil {
			log.Fatal(err)
		}
		w.Write(b)
	})

	mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Add("Content-Type", "text/html; charset=utf-8")
		w.Header().Add("Set-Cookie", fmt.Sprintf("outer=%s; SameSite=Strict", started))
		w.Write([]byte(`
<html>
<head>
</head>
<body>
	hello
	NESTED:
	<iframe src="/nested"></iframe>
	SANDBOX:
	<iframe src="/sandboxed?token" sandbox="allow-scripts"></iframe>
</body>
</html>
		`))
	})

	mux.HandleFunc("/nested", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Add("Content-Type", "text/html; charset=utf-8")
		w.Write([]byte(fmt.Sprintf(`
<html>
<head>
</head>
<body>
	hello nested
	<div id="huh"></div>
	<script>
		async function f() {
			const resp = await fetch("/endpoint");
			console.log(resp);
			const val = await resp.json();
			console.log(val);
			document.getElementById("huh").innerHTML = "fetched: " + val;
		}
		f();
	</script>
	%s
</body>
</html>
		`, r.Header.Get("Cookie"))))
	})

	mux.HandleFunc("/sandboxed", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Add("Content-Type", "text/html; charset=utf-8")
		w.Header().Add("Content-Security-Policy", "sandbox allow-scripts")
		w.Header().Add("Set-Cookie", fmt.Sprintf("sandboxed=%s; SameSite=Strict", started))
		w.Write([]byte(`
<html>
<head>
</head>
<body>
	hello sandboxed
	<div id="huh"></div>
	<script>
		async function f() {
			const resp = await fetch("/endpoint");
			console.log(resp);
			const val = await resp.json();
			console.log(val);
			document.getElementById("huh").innerHTML = "fetched: " + val;
		}
		f();
	</script>
	NESTED SANDBOX:
	<iframe src="/nested"></iframe>
</body>
</html>
		`))
	})

	http.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		log.Println(r.URL.String())
		keys := slices.Collect(maps.Keys(r.Header))
		slices.Sort(keys)
		for _, k := range keys {
			log.Println(k, r.Header.Get(k))
		}
		mux.ServeHTTP(w, r)
	})
	log.Println(http.ListenAndServe(":9090", nil))
}
