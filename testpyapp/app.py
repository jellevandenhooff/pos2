import wit_world

from wit_world.imports.custom_host import huh

class WitWorld(wit_world.WitWorld):
    def hello_world(self) -> str:
        return "snakes in my wasm " + huh()
