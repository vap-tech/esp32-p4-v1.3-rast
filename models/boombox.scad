use <speaker_accent_ring.scad>
// Integrated display + two separate speaker enclosures. All dimensions mm.
// Design coordinates: X horizontal, Y along front face upwards, Z into case.
// Assembly and body rest on XY at world Z=0. Front and rear walls slope equally towards each other.
part = "assembly"; // [assembly,body,left_pod,right_pod,rear_brace,left_lid,right_lid]
show_display = true;
show_speakers = true;
show_accent_rings = true;
accent_flange_d=68;
accent_flange_h=1.2;
accent_seat_h=2.8;
show_brace = true;
show_rear_lids = true;
rear_lid_t=4;
rear_lid_fit=0.3;
rear_lid_insert_d=4.2; // Same successful fit as original enclosure.
rear_lid_insert_depth=8;
rear_lid_border=3;
rear_lid_lip=2;
$fn=64;
angle=-15;
wall=3;
body_radius=3; // External edge radius; 0 restores sharp enclosure.
pod_w=72;
pod_rear_taper=0;
depth=58;
rear_top_drop=0;

// Dimensions carried over from original stand; standoff size still provisional.
board_w=182.5;
display_side_gap=0.5;
center_w=board_w+2*display_side_gap;
board_h=114;
board_t=8;
board_corner_r=6; // Preview display outline only.
board_bottom=5; // Lower edge is 5 mm below display.
board_clearance=0.6;
mount_dx=167;
mount_dy=100;
mount_bottom=7;
peg_d=6;
peg_length=10;
peg_fit=0.4;
mount_t=4;
// Horizontal top meets highest point of upper ears (their front upper edge).
face_h=board_bottom+mount_bottom+mount_dy+6+board_t*tan(angle);
screwdriver_d=7; // Tool access through floor to lower M2 display screws.
speaker_od=64.15;
speaker_cutout=51.71;
speaker_depth=29;
speaker_rim=3.2;
speaker_bcd=56.49;
speaker_screw_d=2.5;
speaker_pilot=10;
function outer_x(n)=center_w/2+pod_w-pod_rear_taper*n/depth;
function bottom_y(n)=-n*tan(angle);
function top_y(n)=face_h-n*tan(angle)-rear_top_drop*n/depth;
function speaker_x()=center_w/2+pod_w/2;
// World bottom plane lies at Z=0.
module upright() { rotate([90-angle,0,0]) children(); }
module side(s=1) { if(s<0) mirror([1,0,0]) children(); else children(); }
// Preserve previous upper depth, widen the rear of the footprint.
case_height=face_h*cos(angle);
rear_slope=tan(abs(angle));
foot_depth=depth/cos(angle)+case_height*(rear_slope-tan(angle));
module pod_volume(inset=0, left=center_w/2, right=center_w/2+pod_w) {
    x0=left+inset;
    x1=right-inset;
    z0=inset;
    z1=case_height-inset;
    f0=z0*tan(angle)-inset/cos(angle);
    f1=z1*tan(angle)-inset/cos(angle);
    b0=-foot_depth+z0*rear_slope+inset/cos(angle);
    b1=-foot_depth+z1*rear_slope+inset/cos(angle);
    // Construct in world coordinates; return to front-face design frame.
    rotate([-(90-angle),0,0])
        polyhedron(points=[
            [x0,f0,z0],[x1,f0,z0],[x1,b0,z0],[x0,b0,z0],
            [x0,f1,z1],[x1,f1,z1],[x1,b1,z1],[x0,b1,z1]],
            faces=[[3,2,1,0],[5,6,7,4],[1,5,4,0],
                   [2,6,5,1],[3,7,6,2],[0,4,7,3]],convexity=10);
}
// Erode by normal offsets, then expand: fillets stay inside original bounds.
// Flat front/rear panels and horizontal base remain at their original planes.
module rounded_pod_volume() {
    if(body_radius>0)
        minkowski() {
            pod_volume(body_radius);
            sphere(r=body_radius,$fn=32);
        }
    else pod_volume();
}
module pod() {
    difference() {
        union() {
            difference() { rounded_pod_volume(); pod_volume(wall); }
            for(a=[45:90:315])
                translate([speaker_x()+sin(a)*speaker_bcd/2,
                           face_h/2+cos(a)*speaker_bcd/2,0])
                    cylinder(d=9,h=13);
        }
        translate([speaker_x(),face_h/2,-1]) cylinder(d=speaker_cutout,h=speaker_depth+2);
        for(a=[45:90:315])
            translate([speaker_x()+sin(a)*speaker_bcd/2,
                       face_h/2+cos(a)*speaker_bcd/2,-0.01])
                cylinder(d=speaker_screw_d,h=speaker_pilot+0.01);
        // Wire into the open display cavity. Seal after wiring.
        translate([center_w/2-1,35-2.5,35-2.5]) cube([wall+2,5,5]);
    }
}
module display_mounts() {
    for(x=[mount_dx/2]) for(y=[board_bottom+mount_bottom,
                                        board_bottom+mount_bottom+mount_dy]) {
        difference() {
            hull() {
                translate([x,y,board_t]) cylinder(d=12,h=peg_length+mount_t);
                translate([center_w/2-1,y-6,board_t])
                    cube([wall+1,12,peg_length+mount_t]);
            }
            // Display standoffs slide into these sockets, seat at their ends.
            translate([x,y,board_t-0.01]) cylinder(d=peg_d+peg_fit,h=peg_length+0.01);
            translate([x,y,board_t-1]) cylinder(d=2.4,h=peg_length+mount_t+2);
            // M2 cylindrical head, rear access.
            translate([x,y,board_t+peg_length+mount_t-2.2]) cylinder(d=4.2,h=3);
        }
    }
}
// Rear frame: X across, Y upwards along rear wall, Z outwards behind case.
brace_bottom=7; // Lower ear stays ~3 mm above table after recess.
brace_h=32;
brace_t=4;
brace_corner_r=3; // Outline only; thickness edges remain square.
brace_w=center_w-1; // Fits between columns, 0.5 mm gap at either side.
brace_mount_x=mount_dx/2;
brace_gap=0.3;
brace_recess=brace_t+brace_gap; // Outer face flush with rear wall.
brace_insert_d=4.2;
brace_insert_depth=8;
module rear_frame() {
    translate([0,-foot_depth,0]) rotate([90-abs(angle),0,0]) children();
}
module brace_mount_positions(right_only=false) {
    for(x=right_only ? [brace_mount_x] : [-brace_mount_x,brace_mount_x])
        for(y=[brace_bottom+6,brace_bottom+brace_h-6])
            translate([x,y,0]) children();
}
module right_brace_ears() {
    brace_mount_positions(true) hull() {
        translate([0,0,-10]) cylinder(d=12,h=10);
        translate([center_w/2-1-brace_mount_x,-6,-10])
            cube([wall+1,12,10]);
    }
}
function rear_height()=case_height/cos(abs(angle));
module rear_corner_positions() {
    for(x=[center_w/2+8,center_w/2+pod_w-8])
        for(y=[8,rear_height()-8]) translate([x,y,0]) children();
}
module rear_opening() {
    rear_frame()
        translate([center_w/2+rear_lid_border,rear_lid_border,-rear_lid_t-0.2])
            cube([pod_w-2*rear_lid_border,rear_height()-2*rear_lid_border,rear_lid_t+2]);
}
module rear_seat() {
    intersection() {
    upright() rounded_pod_volume();
    rear_frame() {
        // Inward ledge supports the inserted lid; local supports allowed in slicer.
        difference() {
            translate([center_w/2,0,-rear_lid_t-2])
                cube([pod_w,rear_height(),2]);
            translate([center_w/2+rear_lid_border+rear_lid_lip,
                       rear_lid_border+rear_lid_lip,-rear_lid_t-3])
                cube([pod_w-2*(rear_lid_border+rear_lid_lip),
                      rear_height()-2*(rear_lid_border+rear_lid_lip),4]);
        }
        // Tie each boss into both perimeter walls.
        rear_corner_positions() translate([0,0,-rear_lid_t-rear_lid_insert_depth-2])
            cylinder(d=14,h=rear_lid_insert_depth+2);
    }
    }
}
module right_pod() {
    difference() {
        union() {
            difference() { pod(); rotate([-(90-angle),0,0]) rear_opening(); }
            display_mounts();
            rotate([-(90-angle),0,0]) {
                rear_frame() translate([0,0,-brace_recess]) right_brace_ears();
                rear_seat();
            }
        }
        rotate([-(90-angle),0,0]) rear_frame() {
            brace_mount_positions(true)
                translate([0,0,-brace_recess-brace_insert_depth])
                    cylinder(d=brace_insert_d,h=brace_insert_depth+0.02);
            rear_corner_positions()
                translate([0,0,-rear_lid_t-rear_lid_insert_depth])
                    cylinder(d=rear_lid_insert_d,h=rear_lid_insert_depth+0.02);
        }
    }
}
module right_lid_flat() {
    difference() {
        translate([center_w/2+rear_lid_border+rear_lid_fit,
                   rear_lid_border+rear_lid_fit,0])
            cube([pod_w-2*(rear_lid_border+rear_lid_fit),
                  rear_height()-2*(rear_lid_border+rear_lid_fit),rear_lid_t]);
        rear_corner_positions() {
            translate([0,0,-1]) cylinder(d=3.2,h=rear_lid_t+2);
            translate([0,0,rear_lid_t-2.2]) cylinder(d=6,h=3);
        }
    }
}
module right_lid() { rear_frame() translate([0,0,-rear_lid_t]) right_lid_flat(); }
module body() { right_pod(); side(-1) right_pod(); }
module brace_flat() {
    difference() {
        translate([-brace_w/2,brace_bottom,0])
            linear_extrude(height=brace_t)
                translate([brace_corner_r,brace_corner_r])
                    offset(r=brace_corner_r)
                        square([brace_w-2*brace_corner_r,brace_h-2*brace_corner_r]);
        brace_mount_positions() {
            translate([0,0,-1]) cylinder(d=3.2,h=brace_t+2);
            translate([0,0,brace_t-2.2]) cylinder(d=6,h=3);
        }
    }
}
module rear_brace() { rear_frame() translate([0,0,brace_gap-brace_recess]) brace_flat(); }
module equipment() {
    if(show_display) color([0.08,0.2,0.25,0.65]) {
        translate([-board_w/2,board_bottom,0])
            linear_extrude(height=board_t)
                translate([board_corner_r,board_corner_r])
                    offset(r=board_corner_r)
                        square([board_w-2*board_corner_r,board_h-2*board_corner_r]);
        translate([-124.5/2,board_bottom+(board_h-74.6)/2,board_t]) cube([124.5,74.6,9]);
        for(x=[-mount_dx/2,mount_dx/2]) for(y=[board_bottom+7,board_bottom+107])
            translate([x,y,board_t]) cylinder(d=peg_d,h=peg_length);
    }
    if(show_accent_rings) for(s=[-1,1]) side(s) color("DarkOrange")
        translate([speaker_x(),face_h/2,0]) rotate([180,0,0])
            speaker_accent_ring(flange_d=accent_flange_d,
                flange_h=accent_flange_h,seat_h=accent_seat_h);
    if(show_speakers) for(s=[-1,1]) side(s)
        translate([0,0,show_accent_rings ? -accent_flange_h-accent_seat_h : 0])
        color([0.2,0.2,0.2,0.65]) {
        translate([speaker_x(),face_h/2,-speaker_rim]) cylinder(d=speaker_od,h=speaker_rim);
        translate([speaker_x(),face_h/2,0]) cylinder(d=speaker_cutout,h=speaker_depth-speaker_rim);
    }
}
if(part=="assembly") {
    upright() {
        color("LightSlateGray") body();
        equipment();
    }
    if(show_brace) color("SteelBlue") rear_brace();
    if(show_rear_lids) color("Silver") { right_lid(); side(-1) right_lid(); }
} else if(part=="body") upright() body();
else if(part=="right_pod") right_pod();
else if(part=="left_pod") side(-1) right_pod();
else if(part=="rear_brace") translate([0,-brace_bottom,0]) brace_flat();

else if(part=="right_lid") right_lid_flat();
else if(part=="left_lid") side(-1) right_lid_flat();
