import { huh, query } from "component:testapp/custom-host";

export function helloWorld() {
	let rows = query("SELECT * FROM test WHERE id = ?", [{tag: 'int', val: BigInt(1)}]);
	if (rows === undefined) {
		return "no rows" + huh();
	}
	rows = rows.map(row => row.map(value => {
		if (value.tag === "int") {
			return Number(value.val);
		}
		if (value.tag === "text") {
			return value.val
		}
		throw value.tag;
	}));
    return "js js " + huh() + " " + JSON.stringify(rows);
}
