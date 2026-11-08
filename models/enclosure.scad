// ESP32 display stand, dimensions in mm. Front is -Y, up is +Z.
// F5: assembly preview. Select a printable part below before exporting STL.
part = "assembly"; // [assembly,base,lid,left_horn,right_horn,speaker_gasket]
show_hardware = true;
show_speaker_gasket = false; // Enable to preview optional TPU spacer and shifted speaker.
show_lid = true; // Disable to inspect rear pocket and port clearance.
show_battery = true; // Inferred battery envelope, preview only.
$fn = 64;

base_w = 182.5;
base_d = 80;
base_h = 77;
wall = 3;
lid_t = 5;
rear_insert_d = 4.2; // Provisional M3 heat-set pocket: confirm insert outer diameter.
rear_insert_depth = 8;
rear_head_d = 6; // M3 cylindrical socket head, with clearance.
rear_head_depth = 3.2;
rear_boss_offset = 5; // Bosses clipped flush to the enclosure exterior.
board_w = 182.5;
board_h = 114;
board_t = 8;
board_tilt = 15;
board_bottom = base_h + 5; // Rear lower edge clears roof by ~2.9 mm at 15 degrees.
mount_x = 167;
mount_z = 100;
mount_bottom = 7;
// NOT measured again after caliper zero correction: confirm before printing horns.
standoff_h = 10;
standoff_d = 6;
horn_t = 5;
board_head_d = 4.2; // M2 cylindrical socket head clearance.
board_head_depth = 2.2;
horn_w = 15;
horn_foot_w = 15;
horn_foot_front = 20;
horn_foot_back = base_d-lid_t-0.5; // Stop before rear cover with 0.5 mm clearance.
horn_foot_t = 6;
horn_insert_d = 4.2; // Confirm actual heat-set insert outside diameter.
horn_insert_depth = 8;
horn_pad_depth = 12;
horn_screw_y = [49,59]; // Rear insert moved forward to clear rear-cover inserts.
// Uprights moved forward independently of feet and gussets.
horn_forward = 2;
board_front_y = horn_foot_front - (board_t+standoff_h)/cos(board_tilt)
                - (base_h-board_bottom)*tan(board_tilt) - horn_forward;
speaker_outer = 64.15;
speaker_body = 51.71;
speaker_depth = 29;
speaker_rim = 3.2;
speaker_screw_length = 12;
speaker_pilot_depth = speaker_screw_length-speaker_rim+1.2; // 10 mm
speaker_boss_depth = speaker_pilot_depth+3;
speaker_bcd = 56.49;
speaker_mount_angle = 45; // Rotate speaker and its four screws clear of battery pocket.
speaker_z = base_h/2;
speaker_gasket_od = 62.9;
speaker_gasket_t = 2.8;
speaker_gasket_hole_d = 2.5; // Same screw-hole diameter as enclosure.
// Two fixed front ports; rough Helmholtz tuning ~140-150 Hz at 0.8 L net.
// Driver T/S parameters unknown. Length includes the front wall.
port_d = 16;
port_wall = 2;
port_length = 55;
port_x = 77;
// Connector housing not measured: provisional rectangular pass-through.
display_wire_w = 8;
display_wire_d = 5;
wire_exit_x = 45; // Right when viewed from the front/display.
wire_exit_y = 56; // Just forward of battery pocket front wall.
// Battery 1260110: inferred dimensions, +1 mm fit, +15 mm BMS allowance.
battery_l = 110 + 1 + 15;
battery_d = 60 + 1;
battery_h = 12 + 1;
battery_wall = 2;
battery_floor_t = 5;
battery_notch_w = 5;
battery_notch_depth = 3;
battery_notch_x = battery_l/2 - battery_notch_w/2;
battery_back = base_d-lid_t;
battery_front = battery_back-battery_h-battery_wall;
battery_top = base_h-wall;
battery_bottom = battery_top-battery_d-battery_floor_t;

// On rear cover: open upwards, roof of assembled base closes the opening.
module battery_pocket() {
    difference() {
        translate([-battery_l/2-battery_wall,battery_front,battery_bottom])
            cube([battery_l+2*battery_wall,battery_h+battery_wall+0.01,
                  battery_d+battery_floor_t]);
        translate([-battery_l/2,battery_front+battery_wall,battery_bottom+battery_floor_t])
            cube([battery_l,battery_h+1,battery_d+1]);
        // Open-top wire notch, beside roof exit to the display.
        translate([battery_notch_x-battery_notch_w/2,battery_front-1,battery_top-battery_notch_depth])
            cube([battery_notch_w,battery_wall+2,battery_notch_depth+1]);
    }
}

module board_frame() {
    translate([0,board_front_y,board_bottom]) rotate([-board_tilt,0,0]) children();
}
module y_hole(d,length) { rotate([90,0,0]) cylinder(d=d,h=length,center=true); }
module slot_y() {
    hull() for (dx=[-0.5,0.5]) translate([dx,0,0]) y_hole(3,200);
}
module lid_screws() {
    for(x=[-base_w/2+rear_boss_offset,base_w/2-rear_boss_offset])
        for(z=[rear_boss_offset,base_h-rear_boss_offset])
        translate([x,0,z]) children();
}
module horn_screws() {
    for(x=[-mount_x/2,mount_x/2]) for(y=horn_screw_y)
        translate([x,y,0]) children();
}
module port_shells() {
    for(x=[-port_x,port_x]) translate([x,0,speaker_z])
        rotate([-90,0,0]) cylinder(d=port_d+2*port_wall,h=port_length);
}
module port_bores() {
    for(x=[-port_x,port_x]) {
        translate([x,-1,speaker_z]) rotate([-90,0,0]) cylinder(d=port_d,h=port_length+2);
        // 1 mm chamfers on both mouths, no unsupported external lip.
        translate([x,-0.01,speaker_z]) rotate([-90,0,0]) cylinder(d1=port_d+2,d2=port_d,h=1.01);
        translate([x,port_length-1,speaker_z]) rotate([-90,0,0]) cylinder(d1=port_d,d2=port_d+2,h=1.01);
    }
}
module base() {
    difference() {
        union() {
            difference() {
                translate([-base_w/2,0,0]) cube([base_w,base_d-lid_t,base_h]);
                translate([-base_w/2+wall,wall,wall])
                    cube([base_w-2*wall,base_d+1,base_h-2*wall]);
            }
            for(a=[speaker_mount_angle:90:270+speaker_mount_angle])
                translate([sin(a)*speaker_bcd/2,0,speaker_z+cos(a)*speaker_bcd/2])
                    rotate([-90,0,0]) cylinder(d=9,h=speaker_boss_depth);
            // Rear screw rails grow vertically when printed front face down.
            intersection() {
                lid_screws() rotate([-90,0,0]) cylinder(d=10,h=base_d-lid_t);
                translate([-base_w/2,0,0]) cube([base_w,base_d-lid_t,base_h]);
            }
            // Solid pads for inserts installed from outside, through the top.
            for(x=[-mount_x/2,mount_x/2])
                translate([x-horn_foot_w/2,36,base_h-horn_pad_depth])
                    cube([horn_foot_w,39,horn_pad_depth]);
            port_shells();
        }
        translate([0,0,speaker_z]) y_hole(speaker_body,12);
        for(a=[speaker_mount_angle:90:270+speaker_mount_angle])
            translate([sin(a)*speaker_bcd/2,speaker_pilot_depth/2,speaker_z+cos(a)*speaker_bcd/2])
                y_hole(2.5,speaker_pilot_depth+0.02);
        // Heat-set inserts enter from the rear, flush with mating plane.
        lid_screws() {
            translate([0,base_d-lid_t-rear_insert_depth/2,0])
                y_hole(rear_insert_d,rear_insert_depth+0.02);
            translate([0,base_d-lid_t-rear_insert_depth-1,0]) y_hole(3.2,2.02);
        }
        horn_screws() {
            translate([0,0,base_h-horn_insert_depth])
                cylinder(d=horn_insert_d,h=horn_insert_depth+0.02);
            translate([0,0,base_h-horn_insert_depth-2]) cylinder(d=3.2,h=2.02);
        }
        port_bores();
        // Upper centre exit towards display; seal around wire after assembly.
        translate([wire_exit_x-display_wire_w/2,wire_exit_y-display_wire_d/2,base_h-wall-1])
            cube([display_wire_w,display_wire_d,wall+2]);
    }
}
module lid() {
    union() {
    battery_pocket();
    difference() {
        translate([-base_w/2,base_d-lid_t,0]) cube([base_w,lid_t,base_h]);
        lid_screws() {
            translate([0,base_d-lid_t/2,0]) y_hole(3.2,lid_t+2);
            translate([0,base_d-rear_head_depth/2,0])
                y_hole(rear_head_d,rear_head_depth+0.02);
        }
    }
}
}
// Printable orientation: speaker/front plane on Z=0, rear opening upwards.
module front_down() { rotate([90,0,0]) children(); }
module horn(side=1) {
    x=side*mount_x/2;
    difference() {
        union() {
            // Rearward foot; lower end of inclined upright overlaps it.
            translate([x-horn_foot_w/2,horn_foot_front,base_h])
                cube([horn_foot_w,horn_foot_back-horn_foot_front,horn_foot_t]);
            intersection() {
                board_frame() translate([x-horn_w/2,board_t+standoff_h,-25])
                    cube([horn_w,horn_t,137]);
                translate([-base_w,0,base_h]) cube([base_w*2,base_d,200]);
            }
            // Gusset at foot/upright junction.
            hull() {
                translate([x-horn_w/2,horn_foot_front+4,base_h]) cube([horn_w,3,18]);
                translate([x-horn_w/2,41,base_h]) cube([horn_w,3,horn_foot_t]);
            }
        }
        for(y=horn_screw_y) {
            translate([x,y,base_h-1]) cylinder(d=3.2,h=horn_foot_t+2);
            translate([x,y,base_h+horn_foot_t-rear_head_depth])
                cylinder(d=rear_head_d,h=rear_head_depth+1);
        }
        board_frame() for(z=[mount_bottom,mount_bottom+mount_z]) {
            translate([x,board_t+standoff_h+horn_t/2,z]) slot_y();
            // Rear access tunnel continues through any foot/gusset material.
            // Slot-shaped counterbore preserves the horizontal adjustment.
            hull() for(dx=[-0.5,0.5])
                translate([x+dx,board_t+standoff_h+horn_t-board_head_depth,z])
                    rotate([-90,0,0]) cylinder(d=board_head_d,h=100);
        }
    }
}
// Printed flat; optional TPU spacer, not fused to enclosure.
module speaker_gasket() {
    difference() {
        cylinder(d=speaker_gasket_od,h=speaker_gasket_t);
        translate([0,0,-1]) cylinder(d=speaker_body,h=speaker_gasket_t+2);
        for(a=[speaker_mount_angle:90:270+speaker_mount_angle])
            translate([sin(a)*speaker_bcd/2,cos(a)*speaker_bcd/2,-1])
                cylinder(d=speaker_gasket_hole_d,h=speaker_gasket_t+2);
    }
}
module hardware() {
    color([0.18,0.22,0.25,0.45]) board_frame() {
        translate([-board_w/2,0,0]) cube([board_w,board_t,board_h]);
        color([0.1,0.5,0.3,0.6]) translate([-124.5/2,board_t,(board_h-74.6)/2])
            cube([124.5,9,74.6]);
        // Connector locations await clarification of reference edges; omitted.
        for(x=[-mount_x/2,mount_x/2]) for(z=[7,107])
            translate([x,board_t+standoff_h/2,z]) y_hole(standoff_d,standoff_h);
    }
    translate([0,show_speaker_gasket ? -speaker_gasket_t : 0,0])
    color([0.22,0.22,0.22,0.65]) {
        difference() {
            translate([0,0,speaker_z]) rotate([90,0,0]) cylinder(d=speaker_outer,h=speaker_rim);
            translate([0,0,speaker_z]) y_hole(speaker_body-5,10);
        }
        translate([0,0,speaker_z]) rotate([-90,0,0]) cylinder(d=speaker_body,h=speaker_depth-speaker_rim);
    }
}

if(part=="assembly") {
    color("LightSlateGray") base();
    if(show_lid) color([0.7,0.75,0.8,0.65]) lid();
    if(show_battery && show_lid) color([0.45,0.65,0.85,0.5])
        translate([-battery_l/2,battery_front+battery_wall,battery_bottom+battery_floor_t])
            cube([battery_l,battery_h,battery_d]);
    color("orange") { horn(-1); horn(1); }
    if(show_speaker_gasket) color("MediumPurple")
        translate([0,0,speaker_z]) rotate([90,0,0]) speaker_gasket();
    if(show_hardware) hardware();
} else if(part=="base") front_down() base();
else if(part=="lid") translate([0,0,base_d]) rotate([-90,0,0]) lid();
else if(part=="left_horn") translate([0,0,-base_h]) horn(-1);
else if(part=="right_horn") translate([0,0,-base_h]) horn(1);

else if(part=="speaker_gasket") speaker_gasket();
