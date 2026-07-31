"""Render a synchronized four-view panel source from the checked proxy clip."""

import argparse
import sys
from pathlib import Path

import bpy
from mathutils import Vector

VIEW_POSITIONS = {
    "front": Vector((0.0, -7.0, 1.8)),
    "side": Vector((7.0, 0.0, 1.8)),
    "back": Vector((0.0, 7.0, 1.8)),
    "three_quarter": Vector((5.0, -5.0, 1.8)),
}
TARGET = Vector((0.0, 0.0, 1.8))
ORTHO_SCALE = 4.8
VIEW_SIZE = 256


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--clip", default="run")
    parser.add_argument("--frames", type=int, default=16)
    script_arguments = sys.argv[sys.argv.index("--") + 1 :] if "--" in sys.argv else []
    return parser.parse_args(script_arguments)


def main() -> None:
    arguments = parse_arguments()
    if arguments.frames < 2 or arguments.frames > 256:
        raise ValueError("--frames must be within 2..=256")

    bpy.ops.wm.read_factory_settings(use_empty=True)
    bpy.ops.import_scene.gltf(filepath=str(arguments.source))
    scene = bpy.context.scene
    scene.render.engine = "BLENDER_WORKBENCH"
    scene.display.render_aa = "8"
    scene.display.shading.light = "STUDIO"
    scene.display.shading.show_shadows = True
    scene.display.shading.show_cavity = True
    scene.display.shading.background_type = "WORLD"
    scene.display.shading.color_type = "MATERIAL"
    scene.render.resolution_x = VIEW_SIZE
    scene.render.resolution_y = VIEW_SIZE
    scene.render.resolution_percentage = 100
    scene.render.image_settings.file_format = "PNG"
    scene.render.film_transparent = False
    scene.world = bpy.data.worlds.new("VideoMotionWorld")
    scene.world.color = (0.92, 0.92, 0.92)

    armature = next(item for item in scene.objects if item.type == "ARMATURE")
    armature.animation_data_create()
    armature.animation_data.action = bpy.data.actions[arguments.clip]

    camera_data = bpy.data.cameras.new("VideoMotionCamera")
    camera_data.type = "ORTHO"
    camera_data.ortho_scale = ORTHO_SCALE
    camera = bpy.data.objects.new("VideoMotionCamera", camera_data)
    scene.collection.objects.link(camera)
    scene.camera = camera

    for view_id, position in VIEW_POSITIONS.items():
        directory = arguments.output / view_id
        directory.mkdir(parents=True, exist_ok=True)
        camera.location = position
        camera.rotation_euler = (TARGET - position).to_track_quat("-Z", "Y").to_euler()
        for frame_index in range(arguments.frames):
            scene.frame_set(frame_index)
            scene.render.filepath = str(directory / f"frame-{frame_index:03}.png")
            bpy.ops.render.render(write_still=True)


main()
